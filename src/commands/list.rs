//! List command - Show all Cursor projects

use anyhow::{Context, Result};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};
use percent_encoding::percent_decode_str;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;
use url::Url;

use super::utils;
use crate::config;

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
    /// The workspace storage folder ID (hash) used by Cursor
    pub folder_id: String,

    /// The project path as stored in workspace.json
    pub path: PathBuf,

    /// Remote connection info (None for local projects)
    pub remote: Option<RemoteInfo>,

    /// When the project was last modified
    pub last_modified: Option<SystemTime>,

    /// Number of chat sessions found
    pub chat_count: usize,
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

        // workspace.json can have either:
        // - "folder": single-folder project
        // - "workspace": multi-root .code-workspace file
        let folder_url = match workspace.get("folder").and_then(|v| v.as_str()) {
            Some(url) => url,
            None => {
                // Multi-root workspace - skip for now (would need to parse .code-workspace)
                // Could also use workspace.get("workspace") to show the workspace file
                continue;
            }
        };

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

        // Count chat sessions (default to 0 on error to avoid one bad project
        // breaking the entire list - the project path is still shown)
        let chat_count = utils::count_chat_sessions(&project_dir).unwrap_or(0);

        projects.push(Project {
            folder_id,
            path: parsed.path,
            remote: parsed.remote,
            last_modified,
            chat_count,
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

/// Options for the list command
pub struct ListOptions {
    /// Show workspace ID for each project
    pub with_id: bool,
    /// Sort by: name, modified, chats
    pub sort: String,
    /// Reverse sort order
    pub reverse: bool,
    /// Filter: local, remote, or pattern to match path
    pub filter: Option<String>,
    /// Limit number of results
    pub limit: Option<usize>,
}

/// Execute the list command and return formatted output
pub fn execute(options: ListOptions) -> Result<String> {
    let workspace_storage_dir = config::workspace_storage_dir()
        .context("Failed to determine workspace storage directory")?;

    let mut projects = list(workspace_storage_dir)?;

    // Apply filter
    if let Some(ref filter_str) = options.filter {
        projects.retain(|p| {
            let path_str = p.path.to_string_lossy();
            match filter_str.as_str() {
                "local" => p.remote.is_none(),
                "remote" => p.remote.is_some(),
                pattern => path_str.contains(pattern),
            }
        });
    }

    // Apply sorting
    match options.sort.as_str() {
        "name" => {
            projects.sort_by(|a, b| a.path.cmp(&b.path));
        }
        "chats" => {
            projects.sort_by(|a, b| b.chat_count.cmp(&a.chat_count));
        }
        _ => {
            // Default (including "modified"): already sorted by modified in list()
        }
    }

    // Reverse if requested
    if options.reverse {
        projects.reverse();
    }

    // Apply limit
    let total_count = projects.len();
    if let Some(n) = options.limit {
        projects.truncate(n);
    }

    // Build table
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic);

    // Build header
    let mut header = vec![];
    if options.with_id {
        header.push(Cell::new("ID"));
    }
    header.push(Cell::new("Remote"));
    header.push(Cell::new("Path"));
    header.push(Cell::new("Chats"));
    header.push(Cell::new("Modified"));
    table.set_header(header);

    for project in &projects {
        let path_str = project.path.to_string_lossy().to_string();
        let chat_str = project.chat_count.to_string();
        let remote_str = match &project.remote {
            Some(r) => format!("{}:{}", r.remote_type, r.name),
            None => "-".to_string(),
        };
        let modified_str = project
            .last_modified
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| {
                let secs = d.as_secs();
                let dt = chrono::DateTime::from_timestamp(secs as i64, 0).unwrap_or_default();
                dt.format("%Y-%m-%d %H:%M").to_string()
            })
            .unwrap_or_else(|| "-".to_string());

        let mut row = vec![];
        if options.with_id {
            row.push(Cell::new(&project.folder_id));
        }
        row.push(Cell::new(remote_str));
        row.push(Cell::new(path_str));
        row.push(Cell::new(chat_str));
        row.push(Cell::new(modified_str));
        table.add_row(row);
    }

    // Build output
    let mut output = table.to_string();
    if projects.len() < total_count {
        output.push_str(&format!(
            "\n\nShowing {} of {} projects",
            projects.len(),
            total_count
        ));
    } else {
        output.push_str(&format!("\n\n{} projects found", total_count));
    }

    Ok(output)
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

    #[cfg(not(windows))]
    #[test]
    fn test_parse_local_url() {
        let parsed = parse_folder_url("file:///Users/me/projects/myapp").unwrap();
        assert_eq!(parsed.path, PathBuf::from("/Users/me/projects/myapp"));
        assert!(parsed.remote.is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn test_parse_local_url_with_spaces() {
        let parsed = parse_folder_url("file:///Users/me/my%20project").unwrap();
        assert_eq!(parsed.path, PathBuf::from("/Users/me/my project"));
        assert!(parsed.remote.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn test_parse_local_url_windows() {
        let parsed = parse_folder_url("file:///C:/Users/me/projects/myapp").unwrap();
        assert_eq!(parsed.path, PathBuf::from("C:\\Users\\me\\projects\\myapp"));
        assert!(parsed.remote.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn test_parse_local_url_with_spaces_windows() {
        let parsed = parse_folder_url("file:///C:/Users/me/my%20project").unwrap();
        assert_eq!(parsed.path, PathBuf::from("C:\\Users\\me\\my project"));
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

    #[test]
    fn test_parse_wsl_url() {
        let parsed = parse_folder_url("vscode-remote://wsl+Ubuntu/home/user/project").unwrap();
        assert_eq!(parsed.path, PathBuf::from("/home/user/project"));
        let remote = parsed.remote.unwrap();
        assert_eq!(remote.remote_type, RemoteType::Wsl);
        assert_eq!(remote.name, "Ubuntu");
    }

    #[test]
    fn test_parse_invalid_scheme() {
        // Unknown scheme should return None
        assert!(parse_folder_url("http://example.com/path").is_none());
        assert!(parse_folder_url("ftp://server/path").is_none());
    }

    #[test]
    fn test_parse_invalid_url() {
        // Malformed URL should return None
        assert!(parse_folder_url("not a url at all").is_none());
    }

    #[test]
    fn test_remote_type_parse() {
        assert_eq!(RemoteType::parse("tunnel"), RemoteType::Tunnel);
        assert_eq!(RemoteType::parse("ssh-remote"), RemoteType::SshRemote);
        assert_eq!(RemoteType::parse("dev-container"), RemoteType::DevContainer);
        assert_eq!(RemoteType::parse("wsl"), RemoteType::Wsl);
        assert_eq!(
            RemoteType::parse("unknown-type"),
            RemoteType::Unknown("unknown-type".to_string())
        );
    }

    #[test]
    fn test_remote_type_display() {
        assert_eq!(format!("{}", RemoteType::Tunnel), "tunnel");
        assert_eq!(format!("{}", RemoteType::SshRemote), "ssh");
        assert_eq!(format!("{}", RemoteType::DevContainer), "container");
        assert_eq!(format!("{}", RemoteType::Wsl), "wsl");
        assert_eq!(
            format!("{}", RemoteType::Unknown("custom".to_string())),
            "custom"
        );
    }

    #[test]
    fn test_project_struct_fields() {
        // Verify Project struct can be constructed with all fields
        let project = Project {
            folder_id: "abc123".to_string(),
            path: PathBuf::from("/test/path"),
            remote: Some(RemoteInfo {
                remote_type: RemoteType::Tunnel,
                name: "myserver".to_string(),
            }),
            last_modified: None,
            chat_count: 5,
        };
        assert_eq!(project.folder_id, "abc123");
        assert_eq!(project.chat_count, 5);
        assert!(project.remote.is_some());
    }
}
