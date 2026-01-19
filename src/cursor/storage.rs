//! Global storage operations
//!
//! Handles updates to ~/Library/Application Support/Cursor/User/globalStorage/storage.json

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Update workspace references in storage.json
///
/// This updates:
/// - backupWorkspaces.folders[].folderUri
/// - profileAssociations.workspaces (key rename)
pub fn update_storage_json<P: AsRef<Path>>(
    storage_path: P,
    old_uri: &str,
    new_uri: &str,
    dry_run: bool,
) -> Result<bool> {
    let storage_path = storage_path.as_ref();

    if !storage_path.exists() {
        return Ok(false);
    }

    let content = fs::read_to_string(storage_path)
        .with_context(|| format!("Failed to read: {}", storage_path.display()))?;

    let mut json: Value = serde_json::from_str(&content).context("Failed to parse storage.json")?;

    // Update backupWorkspaces.folders[].folderUri
    let folders_modified = json
        .get_mut("backupWorkspaces")
        .and_then(|b| b.get_mut("folders"))
        .and_then(|f| f.as_array_mut())
        .map(|arr| {
            arr.iter_mut()
                .filter_map(|folder| folder.get_mut("folderUri"))
                .filter(|uri| uri.as_str() == Some(old_uri))
                .fold(false, |_, uri| {
                    *uri = Value::String(new_uri.to_string());
                    true
                })
        })
        .unwrap_or(false);

    // Update profileAssociations.workspaces (rename key)
    let assoc_modified = json
        .get_mut("profileAssociations")
        .and_then(|a| a.get_mut("workspaces"))
        .and_then(|w| w.as_object_mut())
        .and_then(|obj| obj.remove(old_uri).map(|v| (obj, v)))
        .map(|(obj, value)| {
            obj.insert(new_uri.to_string(), value);
            true
        })
        .unwrap_or(false);

    let modified = folders_modified || assoc_modified;

    if modified && !dry_run {
        let new_content = serde_json::to_string_pretty(&json)?;
        fs::write(storage_path, new_content)
            .with_context(|| format!("Failed to write: {}", storage_path.display()))?;
    }

    Ok(modified)
}

/// A simpler representation of storage.json for reading
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct StorageJson {
    #[serde(rename = "backupWorkspaces")]
    pub backup_workspaces: Option<BackupWorkspaces>,

    #[serde(rename = "profileAssociations")]
    pub profile_associations: Option<ProfileAssociations>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct BackupWorkspaces {
    pub folders: Option<Vec<FolderEntry>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct FolderEntry {
    #[serde(rename = "folderUri")]
    pub folder_uri: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ProfileAssociations {
    pub workspaces: Option<HashMap<String, String>>,
}

impl StorageJson {
    /// Read storage.json from a file
    #[allow(dead_code)]
    pub fn read<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read: {}", path.as_ref().display()))?;
        serde_json::from_str(&content).context("Failed to parse storage.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_update_storage_json() {
        let mut file = NamedTempFile::new().unwrap();
        write!(
            file,
            r#"{{
    "backupWorkspaces": {{
        "folders": [
            {{ "folderUri": "file:///old/path" }},
            {{ "folderUri": "file:///other/path" }}
        ]
    }},
    "profileAssociations": {{
        "workspaces": {{
            "file:///old/path": "__default__profile__"
        }}
    }}
}}"#
        )
        .unwrap();

        let modified =
            update_storage_json(file.path(), "file:///old/path", "file:///new/path", false)
                .unwrap();

        assert!(modified);

        // Verify changes
        let content = fs::read_to_string(file.path()).unwrap();
        assert!(content.contains("file:///new/path"));
        assert!(!content.contains("file:///old/path"));
    }
}
