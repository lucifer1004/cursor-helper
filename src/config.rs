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
}
