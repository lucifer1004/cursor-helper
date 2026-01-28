//! Shared utilities for commands

use anyhow::{Context, Result};
use fs_extra::dir::{self, CopyOptions};
use rusqlite::Connection;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Format bytes as human-readable size
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Copy a directory recursively using fs_extra
pub fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    let options = CopyOptions::new().copy_inside(true);
    dir::copy(src, dst, &options)
        .with_context(|| format!("Failed to copy {} to {}", src.display(), dst.display()))?;
    Ok(())
}

/// Copy directory contents into an existing directory (merge)
pub fn copy_dir_contents(src: &Path, dst: &Path) -> Result<()> {
    let options = CopyOptions::new().content_only(true).overwrite(true);
    dir::copy(src, dst, &options).with_context(|| {
        format!(
            "Failed to copy contents of {} to {}",
            src.display(),
            dst.display()
        )
    })?;
    Ok(())
}

/// Count chat sessions in a workspace directory by querying state.vscdb
pub fn count_chat_sessions(workspace_dir: &Path) -> Result<usize> {
    let db_path = workspace_dir.join("state.vscdb");

    if !db_path.exists() {
        return Ok(0);
    }

    // Open database in read-only mode
    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("Failed to open: {}", db_path.display()))?;

    // Count unique chat session UUIDs from aichat panel keys
    // Pattern: workbench.panel.aichat.{UUID}.*
    let mut stmt = conn
        .prepare("SELECT key FROM ItemTable WHERE key LIKE 'workbench.panel.aichat.%'")
        .with_context(|| "Failed to prepare chat query")?;

    let keys: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .with_context(|| "Failed to query chat sessions")?
        .filter_map(|r| r.ok())
        .collect();

    // Extract unique UUIDs: "workbench.panel.aichat.{UUID}.something" -> UUID
    let uuids: HashSet<_> = keys
        .iter()
        .filter_map(|key| key.strip_prefix("workbench.panel.aichat."))
        .filter_map(|rest| rest.split('.').next())
        .filter(|uuid| !uuid.is_empty())
        .collect();

    Ok(uuids.len())
}

/// Calculate total size of a directory
pub fn calculate_dir_size(path: &Path) -> Result<u64> {
    let mut total = 0;

    for entry in fs::read_dir(path)?.flatten() {
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            total += metadata.len();
        } else if metadata.is_dir() {
            total += calculate_dir_size(&entry.path()).unwrap_or(0);
        }
    }

    Ok(total)
}

/// Find workspace storage directory for a project path
///
/// Supports both local paths and remote paths:
/// - Local: matches file:// URLs in workspace.json
/// - Remote: if path doesn't exist locally, searches vscode-remote:// URLs for matching path component
pub fn find_workspace_dir(project_path: &Path) -> Result<Option<std::path::PathBuf>> {
    let workspace_storage_dir = crate::config::workspace_storage_dir()?;

    if !workspace_storage_dir.exists() {
        return Ok(None);
    }

    // Try local path first
    if project_path.exists() {
        let project_uri = url::Url::from_file_path(project_path)
            .map_err(|_| anyhow::anyhow!("Invalid project path"))?
            .to_string();
        let project_uri_normalized = normalize_uri_for_comparison(&project_uri);

        // Scan workspace storage for matching local project
        for entry in fs::read_dir(&workspace_storage_dir)?.flatten() {
            if !entry.file_type()?.is_dir() {
                continue;
            }

            let workspace_json = entry.path().join("workspace.json");
            if !workspace_json.exists() {
                continue;
            }

            let content = fs::read_to_string(&workspace_json)?;
            let ws: serde_json::Value = serde_json::from_str(&content)?;

            if let Some(folder) = ws.get("folder").and_then(|v| v.as_str()) {
                let folder_normalized = normalize_uri_for_comparison(folder);
                if folder_normalized == project_uri_normalized {
                    return Ok(Some(entry.path()));
                }
            }
        }
    }

    // Path doesn't exist locally - search for matching remote workspace
    // The path might be a remote path like /home/user/project
    let search_path = project_path.to_string_lossy();
    let search_path_normalized = search_path.trim_end_matches('/');

    for entry in fs::read_dir(&workspace_storage_dir)?.flatten() {
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let workspace_json = entry.path().join("workspace.json");
        if !workspace_json.exists() {
            continue;
        }

        let content = fs::read_to_string(&workspace_json)?;
        let ws: serde_json::Value = serde_json::from_str(&content)?;

        if let Some(folder) = ws.get("folder").and_then(|v| v.as_str()) {
            // Check if this is a remote URL and extract the path
            if let Ok(url) = url::Url::parse(folder) {
                if url.scheme() == "vscode-remote" {
                    // Extract path from remote URL and compare
                    let remote_path = url.path().trim_end_matches('/');
                    if remote_path == search_path_normalized {
                        return Ok(Some(entry.path()));
                    }
                    // Also try matching just the final component (project name)
                    if let Some(remote_name) = remote_path.rsplit('/').next() {
                        if let Some(search_name) = search_path_normalized.rsplit(['/', '\\']).next()
                        {
                            if remote_name == search_name && !remote_name.is_empty() {
                                return Ok(Some(entry.path()));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(None)
}

/// Normalize a file URI for comparison
/// On Windows, Cursor uses lowercase drive letters and percent-encoded colons,
/// while Url::from_file_path uses uppercase. We normalize both to lowercase.
fn normalize_uri_for_comparison(uri: &str) -> String {
    #[cfg(windows)]
    {
        normalize_uri_windows(uri)
    }

    #[cfg(not(windows))]
    {
        uri.trim_end_matches('/').to_string()
    }
}

/// Windows-specific URI normalization (public for testing)
/// Normalizes case and percent-encoded colons for comparison
#[doc(hidden)]
pub fn normalize_uri_windows(uri: &str) -> String {
    uri.trim_end_matches('/').to_lowercase().replace("%3a", ":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn test_normalize_uri_windows_case_insensitive() {
        // Windows normalization: URIs should match regardless of drive letter case
        let upper = normalize_uri_windows("file:///C:/path/to/project");
        let lower = normalize_uri_windows("file:///c:/path/to/project");
        assert_eq!(upper, lower);
    }

    #[test]
    fn test_normalize_uri_windows_percent_encoding() {
        // Windows normalization: Cursor stores %3A for colon, Url::from_file_path uses :
        let encoded = normalize_uri_windows("file:///c%3A/path/to/project");
        let decoded = normalize_uri_windows("file:///c:/path/to/project");
        assert_eq!(encoded, decoded);
    }

    #[test]
    fn test_normalize_uri_windows_trailing_slash() {
        let with_slash = normalize_uri_windows("file:///c:/path/");
        let without_slash = normalize_uri_windows("file:///c:/path");
        assert_eq!(with_slash, without_slash);
    }

    #[test]
    fn test_find_workspace_dir_nonexistent() {
        // Non-existent path should return None, not error
        let result = find_workspace_dir(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
