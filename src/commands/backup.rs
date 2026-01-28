//! Backup command - Backup Cursor metadata for a project

use anyhow::{bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use owo_colors::OwoColorize;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use tar::Builder;

use super::utils;
use crate::config;
use crate::cursor::{folder_id, workspace};

/// Backup metadata
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct BackupManifest {
    /// Version of the backup format
    pub version: u32,
    /// Original project path
    pub project_path: String,
    /// Folder ID used for ~/.cursor/projects/
    pub folder_id: String,
    /// Workspace hash used for workspaceStorage/
    pub workspace_hash: String,
    /// Timestamp of backup creation
    pub created_at: i64,
    /// What was included in the backup
    pub includes: BackupContents,
}

/// What's included in the backup
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct BackupContents {
    /// Whether workspaceStorage was included
    pub workspace_storage: bool,
    /// Whether projects data was included
    pub projects_data: bool,
}

/// Execute the backup command
pub fn execute(project_path: &str, backup_file: &str) -> Result<()> {
    let project_path = PathBuf::from(project_path);

    if !project_path.exists() {
        bail!("Project path does not exist: {}", project_path.display());
    }

    // Normalize path
    let project_path = project_path
        .canonicalize()
        .with_context(|| format!("Failed to resolve path: {}", project_path.display()))?;

    // Compute identifiers
    let folder_id = folder_id::path_to_folder_id(&project_path);
    let workspace_hash = workspace::compute_workspace_hash(&project_path)?;

    // Get directories
    let cursor_projects_dir = config::cursor_projects_dir()?;
    let workspace_storage_dir = config::workspace_storage_dir()?;

    let projects_dir = cursor_projects_dir.join(&folder_id);
    let workspace_dir = workspace_storage_dir.join(&workspace_hash);

    // Check what exists
    let has_projects = projects_dir.exists();
    let has_workspace = workspace_dir.exists();

    if !has_projects && !has_workspace {
        bail!("No Cursor data found for: {}", project_path.display());
    }

    println!("Creating backup for: {}", project_path.display());
    println!("  Folder ID: {}", folder_id);
    println!("  Workspace hash: {}", workspace_hash);
    println!();

    if has_projects {
        println!("{} projects/ data", "Found:".green());
    }
    if has_workspace {
        println!("{} workspaceStorage/ data", "Found:".green());
    }
    println!();

    // Create backup manifest
    let manifest = BackupManifest {
        version: 1,
        project_path: project_path.to_string_lossy().to_string(),
        folder_id: folder_id.clone(),
        workspace_hash: workspace_hash.clone(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        includes: BackupContents {
            workspace_storage: has_workspace,
            projects_data: has_projects,
        },
    };

    // Create tar.gz archive
    let backup_path = if backup_file.ends_with(".tar.gz") {
        PathBuf::from(backup_file)
    } else {
        PathBuf::from(format!("{}.tar.gz", backup_file))
    };

    let file = File::create(&backup_path)
        .with_context(|| format!("Failed to create: {}", backup_path.display()))?;

    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = Builder::new(encoder);

    // Add manifest
    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    add_file_to_archive(&mut archive, "manifest.json", manifest_json.as_bytes())?;

    // Add workspace storage
    if has_workspace {
        println!("Adding workspaceStorage/...");
        add_dir_to_archive(&mut archive, &workspace_dir, "workspaceStorage")?;
    }

    // Add projects data
    if has_projects {
        println!("Adding projects/...");
        add_dir_to_archive(&mut archive, &projects_dir, "projects")?;
    }

    // Finish archive
    let encoder = archive.into_inner()?;
    encoder.finish()?;

    // Get file size
    let size = fs::metadata(&backup_path)?.len();

    println!();
    println!(
        "{} {} ({})",
        "Created:".green(),
        backup_path.display(),
        utils::format_size(size)
    );

    Ok(())
}

/// Add a file with content to the archive
fn add_file_to_archive<W: Write>(
    archive: &mut Builder<W>,
    name: &str,
    content: &[u8],
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    );
    header.set_cksum();

    archive.append_data(&mut header, name, content)?;
    Ok(())
}

/// Add a directory recursively to the archive
fn add_dir_to_archive<W: Write>(
    archive: &mut Builder<W>,
    source: &Path,
    prefix: &str,
) -> Result<()> {
    for entry in walkdir::WalkDir::new(source)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let relative = path
            .strip_prefix(source)
            .with_context(|| format!("Failed to strip prefix from: {}", path.display()))?;

        let archive_path = if relative.as_os_str().is_empty() {
            PathBuf::from(prefix)
        } else {
            PathBuf::from(prefix).join(relative)
        };

        if path.is_dir() {
            archive.append_dir(&archive_path, path)?;
        } else if path.is_file() {
            archive.append_path_with_name(path, &archive_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backup_manifest_serialization() {
        let manifest = BackupManifest {
            version: 1,
            project_path: "/home/user/project".to_string(),
            folder_id: "home-user-project".to_string(),
            workspace_hash: "abc123def456".to_string(),
            created_at: 1704067200,
            includes: BackupContents {
                workspace_storage: true,
                projects_data: true,
            },
        };

        // Should serialize to JSON without error
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains("\"version\":1"));
        assert!(json.contains("\"project_path\":\"/home/user/project\""));
        assert!(json.contains("\"workspace_storage\":true"));
    }

    #[test]
    fn test_backup_manifest_deserialization() {
        let json = r#"{
            "version": 1,
            "project_path": "/test/path",
            "folder_id": "test-path",
            "workspace_hash": "hash123",
            "created_at": 1704067200,
            "includes": {
                "workspace_storage": true,
                "projects_data": false
            }
        }"#;

        let manifest: BackupManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.project_path, "/test/path");
        assert!(manifest.includes.workspace_storage);
        assert!(!manifest.includes.projects_data);
    }

    #[test]
    fn test_backup_contents_default() {
        let contents = BackupContents {
            workspace_storage: false,
            projects_data: false,
        };
        assert!(!contents.workspace_storage);
        assert!(!contents.projects_data);
    }
}
