//! List command - Show all Cursor projects

use anyhow::{Context, Result};
use percent_encoding::percent_decode_str;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;
use url::Url;

use super::utils;

/// Remote connection type for vscode-remote:// URLs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteType {
    Tunnel,
    SshRemote,
    DevContainer,
    Wsl,
    Unknown(String),
}

impl RemoteType {
    fn parse(s: &str) -> Self {
        match s {
            "tunnel" => Self::Tunnel,
            "ssh-remote" => Self::SshRemote,
            "dev-container" => Self::DevContainer,
            "wsl" => Self::Wsl,
            other => Self::Unknown(other.to_string()),
        }
    }
}

impl std::fmt::Display for RemoteType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tunnel => write!(f, "tunnel"),
            Self::SshRemote => write!(f, "ssh"),
            Self::DevContainer => write!(f, "container"),
            Self::Wsl => write!(f, "wsl"),
            Self::Unknown(s) => write!(f, "{}", s),
        }
    }
}

/// Remote connection info for vscode-remote:// URLs
#[derive(Debug, Clone)]
pub struct RemoteInfo {
    /// Remote type
    pub remote_type: RemoteType,
    /// Remote host/name
    pub name: String,
}

/// A Cursor project discovered from workspace storage
#[derive(Debug)]
pub struct Project {
    /// The folder ID (hash) used by Cursor
    pub folder_id: String,

    /// The project path as stored in workspace.json
    pub path: PathBuf,

    /// Remote connection info (None for local projects)
    pub remote: Option<RemoteInfo>,

    /// When the project was last modified
    pub last_modified: Option<SystemTime>,

    /// Number of chat sessions found
    pub chat_count: usize,

    /// The workspace hash (same as folder_id)
    #[allow(dead_code)]
    pub workspace_hash: String,
}

/// List all Cursor projects
pub fn list(workspace_storage_dir: PathBuf) -> Result<Vec<Project>> {
    let mut projects = Vec::new();

    if !workspace_storage_dir.exists() {
        return Ok(projects);
    }

    // Read all entries in workspace storage
    let entries = fs::read_dir(&workspace_storage_dir)
        .with_context(|| format!("Failed to read: {}", workspace_storage_dir.display()))?;

    for entry in entries.flatten() {
        // Skip non-directory entries
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let folder_id = entry.file_name().to_string_lossy().to_string();
        let project_dir = entry.path();

        // Try to read workspace.json
        let workspace_json_path = project_dir.join("workspace.json");
        if !workspace_json_path.exists() {
            continue;
        }

        let content = fs::read_to_string(&workspace_json_path)
            .with_context(|| format!("Failed to read: {}", workspace_json_path.display()))?;

        // Parse workspace.json
        let workspace: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse: {}", workspace_json_path.display()))?;

        let folder_url = workspace
            .get("folder")
            .and_then(|v| v.as_str())
            .context("workspace.json missing 'folder' field")?;

        // Parse folder URL
        let parsed = match parse_folder_url(folder_url) {
            Some(p) => p,
            None => {
                eprintln!(
                    "Warning: Invalid folder URL in {}: {}",
                    workspace_json_path.display(),
                    folder_url
                );
                continue;
            }
        };

        // Get last modified time
        let last_modified = entry.metadata()?.modified().ok();

        // Count chat sessions
        let chat_count = utils::count_chat_sessions(&project_dir).unwrap_or(0);

        projects.push(Project {
            folder_id: folder_id.clone(),
            path: parsed.path,
            remote: parsed.remote,
            last_modified,
            chat_count,
            workspace_hash: folder_id,
        });
    }

    // Sort by last modified (most recent first)
    projects.sort_by(|a, b| {
        b.last_modified
            .cmp(&a.last_modified)
            .then_with(|| a.path.cmp(&b.path))
    });

    Ok(projects)
}

/// Parsed URL result containing path and optional remote info
struct ParsedUrl {
    path: PathBuf,
    remote: Option<RemoteInfo>,
}

/// Convert a file:// or vscode-remote:// URL to a PathBuf with optional remote info
fn parse_folder_url(url_str: &str) -> Option<ParsedUrl> {
    let url = Url::parse(url_str).ok()?;

    match url.scheme() {
        "file" => {
            // file:// URL - local project
            let path = url.to_file_path().ok()?;
            Some(ParsedUrl { path, remote: None })
        }
        "vscode-remote" => {
            // vscode-remote://[type]+[name]/path
            // Complex format: dev-container+{config}@ssh-remote+host/path
            //   - username = dev-container+{config}
            //   - host = ssh-remote+host

            // Check for dev-container (appears as username in URL)
            let username = percent_decode_str(url.username()).decode_utf8_lossy();
            let host_encoded = url.host_str()?;
            let host = percent_decode_str(host_encoded).decode_utf8_lossy();

            let remote = if username.starts_with("dev-container+") {
                // Dev container on underlying remote - use host for the underlying name
                let name = host.split('+').nth(1).unwrap_or("container").to_string();
                Some(RemoteInfo {
                    remote_type: RemoteType::DevContainer,
                    name,
                })
            } else if let Some(plus_pos) = host.find('+') {
                // Simple remote: tunnel+name, ssh-remote+host, etc.
                let remote_type_str = &host[..plus_pos];
                let name = host[plus_pos + 1..].to_string();
                Some(RemoteInfo {
                    remote_type: RemoteType::parse(remote_type_str),
                    name,
                })
            } else {
                None
            };

            let path = PathBuf::from(url.path());
            Some(ParsedUrl { path, remote })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_local_url() {
        let parsed = parse_folder_url("file:///Users/me/projects/myapp").unwrap();
        assert_eq!(parsed.path, PathBuf::from("/Users/me/projects/myapp"));
        assert!(parsed.remote.is_none());
    }

    #[test]
    fn test_parse_local_url_with_spaces() {
        let parsed = parse_folder_url("file:///Users/me/my%20project").unwrap();
        assert_eq!(parsed.path, PathBuf::from("/Users/me/my project"));
        assert!(parsed.remote.is_none());
    }

    #[test]
    fn test_parse_tunnel_url() {
        let parsed =
            parse_folder_url("vscode-remote://tunnel+myserver/home/user/data/project").unwrap();
        assert_eq!(parsed.path, PathBuf::from("/home/user/data/project"));
        let remote = parsed.remote.unwrap();
        assert_eq!(remote.remote_type, RemoteType::Tunnel);
        assert_eq!(remote.name, "myserver");
    }

    #[test]
    fn test_parse_ssh_remote_url() {
        let parsed =
            parse_folder_url("vscode-remote://ssh-remote+myhost/home/user/project").unwrap();
        assert_eq!(parsed.path, PathBuf::from("/home/user/project"));
        let remote = parsed.remote.unwrap();
        assert_eq!(remote.remote_type, RemoteType::SshRemote);
        assert_eq!(remote.name, "myhost");
    }

    #[test]
    fn test_parse_percent_encoded_tunnel_url() {
        // Real-world format: + is encoded as %2B
        let parsed =
            parse_folder_url("vscode-remote://tunnel%2Bdev-server/home/user/.config/myapp")
                .unwrap();
        assert_eq!(parsed.path, PathBuf::from("/home/user/.config/myapp"));
        let remote = parsed.remote.unwrap();
        assert_eq!(remote.remote_type, RemoteType::Tunnel);
        assert_eq!(remote.name, "dev-server");
    }

    #[test]
    fn test_parse_dev_container_on_ssh() {
        // Dev container running on SSH remote: dev-container+{config}@ssh-remote+host
        let parsed = parse_folder_url(
            "vscode-remote://dev-container%2Bconfig@ssh-remote%2Bwin11-wsl/workspaces/project",
        )
        .unwrap();
        assert_eq!(parsed.path, PathBuf::from("/workspaces/project"));
        let remote = parsed.remote.unwrap();
        assert_eq!(remote.remote_type, RemoteType::DevContainer);
        assert_eq!(remote.name, "win11-wsl");
    }
}
