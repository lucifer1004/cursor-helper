//! Platform-specific configuration and paths

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Get the Cursor projects directory (~/.cursor/projects/)
pub fn cursor_projects_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".cursor").join("projects"))
}

/// Get the Cursor workspace storage directory
/// - macOS: ~/Library/Application Support/Cursor/User/workspaceStorage/
/// - Linux: ~/.config/Cursor/User/workspaceStorage/
/// - Windows: %APPDATA%/Cursor/User/workspaceStorage/
pub fn workspace_storage_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("Cursor")
            .join("User")
            .join("workspaceStorage"))
    }

    #[cfg(target_os = "linux")]
    {
        let config = dirs::config_dir().context("Could not determine config directory")?;
        Ok(config.join("Cursor").join("User").join("workspaceStorage"))
    }

    #[cfg(target_os = "windows")]
    {
        let appdata = dirs::config_dir().context("Could not determine AppData directory")?;
        Ok(appdata.join("Cursor").join("User").join("workspaceStorage"))
    }
}

/// Get the Cursor global storage directory
/// - macOS: ~/Library/Application Support/Cursor/User/globalStorage/
/// - Linux: ~/.config/Cursor/User/globalStorage/
/// - Windows: %APPDATA%/Cursor/User/globalStorage/
pub fn global_storage_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("Cursor")
            .join("User")
            .join("globalStorage"))
    }

    #[cfg(target_os = "linux")]
    {
        let config = dirs::config_dir().context("Could not determine config directory")?;
        Ok(config.join("Cursor").join("User").join("globalStorage"))
    }

    #[cfg(target_os = "windows")]
    {
        let appdata = dirs::config_dir().context("Could not determine AppData directory")?;
        Ok(appdata.join("Cursor").join("User").join("globalStorage"))
    }
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
