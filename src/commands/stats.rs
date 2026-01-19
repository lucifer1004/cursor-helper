//! Stats command - Show usage statistics for a project

use anyhow::{Context, Result};
use std::path::PathBuf;

use super::utils;
use crate::config;
use crate::cursor::folder_id;

/// Usage statistics for a Cursor project
#[derive(Debug, Default)]
pub struct Stats {
    /// Project path
    pub project_path: PathBuf,

    /// Number of chat sessions
    pub chat_sessions: usize,

    /// Size of workspace storage in bytes
    pub workspace_size: u64,

    /// Size of projects data in bytes
    pub projects_size: u64,

    /// Folder ID (for ~/.cursor/projects/)
    pub folder_id: String,

    /// Workspace hash (for workspaceStorage/)
    pub workspace_hash: Option<String>,
}

/// Get usage statistics for a project
pub fn stats(project_path: Option<PathBuf>) -> Result<Stats> {
    // Default to current directory if no path provided
    let project_path = match project_path {
        Some(p) => p,
        None => std::env::current_dir().context("Failed to get current directory")?,
    };

    // Normalize path
    let project_path = project_path
        .canonicalize()
        .with_context(|| format!("Path does not exist: {}", project_path.display()))?;

    // Compute identifiers
    let folder_id_str = folder_id::path_to_folder_id(&project_path);

    // Get directories
    let cursor_projects_dir = config::cursor_projects_dir()?;
    let projects_dir = cursor_projects_dir.join(&folder_id_str);

    // Find workspace storage
    let workspace_dir = utils::find_workspace_dir(&project_path)?;

    // Calculate sizes
    let projects_size = if projects_dir.exists() {
        utils::calculate_dir_size(&projects_dir).unwrap_or(0)
    } else {
        0
    };

    let (workspace_size, chat_sessions, workspace_hash) = match &workspace_dir {
        Some(dir) => {
            let size = utils::calculate_dir_size(dir).unwrap_or(0);
            let chats = utils::count_chat_sessions(dir).unwrap_or(0);
            let hash = dir.file_name().map(|n| n.to_string_lossy().to_string());
            (size, chats, hash)
        }
        None => (0, 0, None),
    };

    Ok(Stats {
        project_path,
        chat_sessions,
        workspace_size,
        projects_size,
        folder_id: folder_id_str,
        workspace_hash,
    })
}

/// Format stats for display
pub fn format_stats(stats: &Stats) -> String {
    let mut lines = vec![];

    lines.push(format!("Project: {}", stats.project_path.display()));
    lines.push(format!("Folder ID: {}", stats.folder_id));

    if let Some(hash) = &stats.workspace_hash {
        lines.push(format!("Workspace Hash: {}", hash));
    } else {
        lines.push("Workspace Hash: (not found)".to_string());
    }

    lines.push(String::new()); // blank line

    lines.push(format!("Chat Sessions: {}", stats.chat_sessions));
    lines.push(format!(
        "Workspace Storage: {}",
        utils::format_size(stats.workspace_size)
    ));
    lines.push(format!(
        "Projects Data: {}",
        utils::format_size(stats.projects_size)
    ));
    lines.push(format!(
        "Total Cursor Data: {}",
        utils::format_size(stats.workspace_size + stats.projects_size)
    ));

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_default() {
        let stats = Stats::default();
        assert_eq!(stats.chat_sessions, 0);
        assert_eq!(stats.workspace_size, 0);
        assert_eq!(stats.projects_size, 0);
    }
}
