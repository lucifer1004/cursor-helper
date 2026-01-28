//! Platform-specific configuration and paths

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Get the Cursor projects directory (~/.cursor/projects/)
pub fn cursor_projects_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".cursor").join("projects"))
}

/// Get the Cursor configuration directory
/// - macOS: ~/Library/Application Support/Cursor/
/// - Linux: ~/.config/Cursor/
/// - Windows: %APPDATA%/Cursor/
fn cursor_config_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("Cursor"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let config = dirs::config_dir().context("Could not determine config directory")?;
        Ok(config.join("Cursor"))
    }
}

/// Get the Cursor workspace storage directory
/// - macOS: ~/Library/Application Support/Cursor/User/workspaceStorage/
/// - Linux: ~/.config/Cursor/User/workspaceStorage/
/// - Windows: %APPDATA%/Cursor/User/workspaceStorage/
pub fn workspace_storage_dir() -> Result<PathBuf> {
    Ok(cursor_config_dir()?.join("User").join("workspaceStorage"))
}

/// Get the Cursor global storage directory
/// - macOS: ~/Library/Application Support/Cursor/User/globalStorage/
/// - Linux: ~/.config/Cursor/User/globalStorage/
/// - Windows: %APPDATA%/Cursor/User/globalStorage/
pub fn global_storage_dir() -> Result<PathBuf> {
    Ok(cursor_config_dir()?.join("User").join("globalStorage"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paths_exist() {
        // These should not panic
        let _ = cursor_projects_dir();
        let _ = workspace_storage_dir();
        let _ = global_storage_dir();
    }

    #[test]
    fn test_cursor_projects_dir_structure() {
        let path = cursor_projects_dir().unwrap();
        // Should end with .cursor/projects
        let components: Vec<_> = path.components().collect();
        let len = components.len();
        assert!(len >= 2);
        assert_eq!(
            components[len - 1].as_os_str().to_string_lossy(),
            "projects"
        );
        assert_eq!(components[len - 2].as_os_str().to_string_lossy(), ".cursor");
    }

    #[test]
    fn test_workspace_storage_dir_structure() {
        let path = workspace_storage_dir().unwrap();
        // Should end with User/workspaceStorage
        let components: Vec<_> = path.components().collect();
        let len = components.len();
        assert!(len >= 2);
        assert_eq!(
            components[len - 1].as_os_str().to_string_lossy(),
            "workspaceStorage"
        );
        assert_eq!(components[len - 2].as_os_str().to_string_lossy(), "User");
    }

    #[test]
    fn test_global_storage_dir_structure() {
        let path = global_storage_dir().unwrap();
        // Should end with User/globalStorage
        let components: Vec<_> = path.components().collect();
        let len = components.len();
        assert!(len >= 2);
        assert_eq!(
            components[len - 1].as_os_str().to_string_lossy(),
            "globalStorage"
        );
        assert_eq!(components[len - 2].as_os_str().to_string_lossy(), "User");
    }

    #[test]
    fn test_paths_share_common_base() {
        // workspace_storage_dir and global_storage_dir should share base up to User/
        let ws = workspace_storage_dir().unwrap();
        let gs = global_storage_dir().unwrap();

        let ws_parent = ws.parent().unwrap();
        let gs_parent = gs.parent().unwrap();

        assert_eq!(ws_parent, gs_parent);
    }
}
