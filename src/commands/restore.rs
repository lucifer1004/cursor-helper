//! Restore command - Restore Cursor metadata from a backup

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use owo_colors::OwoColorize;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;

use super::backup::BackupManifest;
use super::utils;
use crate::config;
use crate::cursor::{folder_id, workspace};

/// Execute the restore command
pub fn execute(backup_file: &str, new_path: &str) -> Result<()> {
    let backup_path = PathBuf::from(backup_file);
    let new_path = PathBuf::from(new_path);

    if !backup_path.exists() {
        bail!("Backup file does not exist: {}", backup_path.display());
    }

    // New path's parent must exist
    if let Some(parent) = new_path.parent() {
        if !parent.exists() {
            bail!("Parent directory does not exist: {}", parent.display());
        }
    }

    // Read and parse manifest from archive
    let manifest = read_manifest(&backup_path)?;

    println!("Restoring from backup:");
    println!("  Original path: {}", manifest.project_path);
    println!("  New path: {}", new_path.display());
    println!("  Backup version: {}", manifest.version);
    println!();

    // Compute new identifiers
    // For restore, we need the new path to exist first to compute the hash
    // We'll create it if it doesn't exist
    if !new_path.exists() {
        fs::create_dir_all(&new_path)
            .with_context(|| format!("Failed to create: {}", new_path.display()))?;
        println!("{} {}", "Created:".green(), new_path.display());
    }

    let new_folder_id = folder_id::path_to_folder_id(&new_path);
    let new_workspace_hash = workspace::compute_workspace_hash(&new_path)?;

    println!("New identifiers:");
    println!("  Folder ID: {}", new_folder_id);
    println!("  Workspace hash: {}", new_workspace_hash);
    println!();

    // Get directories
    let cursor_projects_dir = config::cursor_projects_dir()?;
    let workspace_storage_dir = config::workspace_storage_dir()?;

    let new_projects_dir = cursor_projects_dir.join(&new_folder_id);
    let new_workspace_dir = workspace_storage_dir.join(&new_workspace_hash);

    // Check for conflicts
    if new_projects_dir.exists() {
        println!(
            "{} projects/ already exists: {}",
            "Warning:".yellow(),
            new_projects_dir.display()
        );
    }
    if new_workspace_dir.exists() {
        println!(
            "{} workspaceStorage/ already exists: {}",
            "Warning:".yellow(),
            new_workspace_dir.display()
        );
    }

    // Extract archive
    println!("Extracting backup...");

    let file = File::open(&backup_path)
        .with_context(|| format!("Failed to open: {}", backup_path.display()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    // Create temp directory for extraction
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;

    archive
        .unpack(temp_dir.path())
        .context("Failed to extract backup")?;

    // Move extracted content to correct locations
    let extracted_workspace = temp_dir.path().join("workspaceStorage");
    let extracted_projects = temp_dir.path().join("projects");

    if extracted_workspace.exists() && manifest.includes.workspace_storage {
        println!("Restoring workspaceStorage/...");

        // Ensure parent exists
        if let Some(parent) = new_workspace_dir.parent() {
            fs::create_dir_all(parent)?;
        }

        // Copy or move
        if new_workspace_dir.exists() {
            // Merge into existing
            utils::copy_dir_contents(&extracted_workspace, &new_workspace_dir)?;
        } else {
            fs::rename(&extracted_workspace, &new_workspace_dir).or_else(|_| {
                // rename might fail across filesystems, use copy instead
                utils::copy_dir(&extracted_workspace, &new_workspace_dir)
            })?;
        }

        // Update workspace.json with new path
        let workspace_json_path = new_workspace_dir.join("workspace.json");
        if workspace_json_path.exists() {
            let ws = workspace::WorkspaceJson::new(&new_path)?;
            ws.write(&workspace_json_path)?;
            println!("  Updated workspace.json with new path");
        }

        println!("  -> {}", new_workspace_dir.display());
    }

    if extracted_projects.exists() && manifest.includes.projects_data {
        println!("Restoring projects/...");

        // Ensure parent exists
        if let Some(parent) = new_projects_dir.parent() {
            fs::create_dir_all(parent)?;
        }

        if new_projects_dir.exists() {
            utils::copy_dir_contents(&extracted_projects, &new_projects_dir)?;
        } else {
            fs::rename(&extracted_projects, &new_projects_dir)
                .or_else(|_| utils::copy_dir(&extracted_projects, &new_projects_dir))?;
        }

        println!("  -> {}", new_projects_dir.display());
    }

    println!();
    println!("{}", "Restore complete!".green());
    println!("You can now open {} in Cursor.", new_path.display());

    Ok(())
}

/// Read manifest from a backup archive
fn read_manifest(backup_path: &Path) -> Result<BackupManifest> {
    let file = File::open(backup_path)
        .with_context(|| format!("Failed to open: {}", backup_path.display()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;

        if path.to_string_lossy() == "manifest.json" {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            let manifest: BackupManifest =
                serde_json::from_str(&content).context("Failed to parse manifest.json")?;
            return Ok(manifest);
        }
    }

    bail!("Backup archive does not contain manifest.json")
}

#[cfg(test)]
mod tests {
    // Integration tests would require actual backup files
}
