//! Clone command - Clone a project with full chat history to a new location
//!
//! Unlike rename, clone:
//! - Creates new UUIDs for all references
//! - Original project remains intact
//! - Both projects have independent chat history

use anyhow::{bail, Context, Result};
use owo_colors::OwoColorize;
use std::path::PathBuf;
use uuid::Uuid;

use super::utils;
use crate::config;
use crate::cursor::{folder_id, workspace};

/// Execute the clone command
pub fn execute(old_path: &str, new_path: &str, dry_run: bool) -> Result<()> {
    let old_path = PathBuf::from(old_path);
    let new_path = PathBuf::from(new_path);

    // Validate paths
    if !old_path.exists() {
        bail!("Source path does not exist: {}", old_path.display());
    }
    if new_path.exists() {
        bail!("Destination path already exists: {}", new_path.display());
    }

    // Normalize old path
    let old_path = old_path
        .canonicalize()
        .with_context(|| format!("Failed to resolve path: {}", old_path.display()))?;

    // Compute old identifiers
    let old_folder_id = folder_id::path_to_folder_id(&old_path);
    let old_workspace_hash = workspace::compute_workspace_hash(&old_path)?;

    // Get directories
    let cursor_projects_dir = config::cursor_projects_dir()?;
    let workspace_storage_dir = config::workspace_storage_dir()?;

    let old_projects_dir = cursor_projects_dir.join(&old_folder_id);
    let old_workspace_dir = workspace_storage_dir.join(&old_workspace_hash);

    // Check what exists
    let has_projects = old_projects_dir.exists();
    let has_workspace = old_workspace_dir.exists();

    if !has_projects && !has_workspace {
        bail!("No Cursor data found for: {}", old_path.display());
    }

    println!("Cloning project:");
    println!("  Source: {}", old_path.display());
    println!("  Destination: {}", new_path.display());
    println!();
    println!("Source identifiers:");
    println!("  Folder ID: {}", old_folder_id);
    println!("  Workspace hash: {}", old_workspace_hash);
    println!();

    if has_projects {
        println!("{} projects/ data", "Found:".green());
    }
    if has_workspace {
        println!("{} workspaceStorage/ data", "Found:".green());
    }
    println!();

    if dry_run {
        println!("{}", "(DRY-RUN) Would perform the following:".blue());
        println!("  1. Copy project folder to new location");
        println!("  2. Create new workspace storage with new hash");
        println!("  3. Copy and update all chat sessions with new UUIDs");
        println!("  4. Update workspace.json with new path");
        return Ok(());
    }

    // Step 1: Copy project folder
    println!("Step 1: Copying project folder...");
    utils::copy_dir(&old_path, &new_path)?;
    println!("  -> {}", new_path.display());

    // Compute new identifiers (after creating the folder)
    let new_folder_id = folder_id::path_to_folder_id(&new_path);
    let new_workspace_hash = workspace::compute_workspace_hash(&new_path)?;

    println!();
    println!("New identifiers:");
    println!("  Folder ID: {}", new_folder_id);
    println!("  Workspace hash: {}", new_workspace_hash);
    println!();

    // Step 2: Clone projects data
    let new_projects_dir = cursor_projects_dir.join(&new_folder_id);
    if has_projects {
        println!("Step 2: Cloning projects/ data...");
        if let Some(parent) = new_projects_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        utils::copy_dir(&old_projects_dir, &new_projects_dir)?;
        println!("  -> {}", new_projects_dir.display());
    } else {
        println!("Step 2: No projects/ data to clone");
    }

    // Step 3: Clone and update workspace storage
    let new_workspace_dir = workspace_storage_dir.join(&new_workspace_hash);
    if has_workspace {
        println!("Step 3: Cloning workspaceStorage/ data...");
        if let Some(parent) = new_workspace_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        utils::copy_dir(&old_workspace_dir, &new_workspace_dir)?;

        // Update workspace.json
        let workspace_json_path = new_workspace_dir.join("workspace.json");
        if workspace_json_path.exists() {
            let ws = workspace::WorkspaceJson::new(&new_path)?;
            ws.write(&workspace_json_path)?;
            println!("  Updated workspace.json");
        }

        // Remap chat session UUIDs in state.vscdb
        let db_path = new_workspace_dir.join("state.vscdb");
        if db_path.exists() {
            let remapped = remap_chat_uuids(&db_path)?;
            if remapped > 0 {
                println!("  Remapped {} chat session UUID(s)", remapped);
            }
        }

        println!("  -> {}", new_workspace_dir.display());
    } else {
        println!("Step 3: No workspaceStorage/ data to clone");
    }

    println!();
    println!("{}", "Clone complete!".green());
    println!();
    println!("Both projects now have independent chat histories:");
    println!("  Original: {}", old_path.display());
    println!("  Clone: {}", new_path.display());

    Ok(())
}

/// Remap chat session UUIDs in the SQLite database
/// This ensures the cloned project has independent chat sessions
fn remap_chat_uuids(db_path: &PathBuf) -> Result<usize> {
    use rusqlite::Connection;
    use std::collections::HashMap;

    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open: {}", db_path.display()))?;

    // Find all aichat keys and their UUIDs
    let mut stmt = conn
        .prepare("SELECT key FROM ItemTable WHERE key LIKE 'workbench.panel.aichat.%'")
        .context("Failed to prepare query")?;

    let keys: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .context("Failed to query")?
        .filter_map(|r| r.ok())
        .collect();

    if keys.is_empty() {
        return Ok(0);
    }

    // Extract unique UUIDs and create mapping to new UUIDs
    let mut uuid_map: HashMap<String, String> = HashMap::new();

    for key in &keys {
        if let Some(rest) = key.strip_prefix("workbench.panel.aichat.") {
            if let Some(old_uuid) = rest.split('.').next() {
                if !old_uuid.is_empty() && !uuid_map.contains_key(old_uuid) {
                    let new_uuid = Uuid::new_v4().to_string();
                    uuid_map.insert(old_uuid.to_string(), new_uuid);
                }
            }
        }
    }

    if uuid_map.is_empty() {
        return Ok(0);
    }

    // Update keys with new UUIDs
    for (old_uuid, new_uuid) in &uuid_map {
        let old_prefix = format!("workbench.panel.aichat.{}.", old_uuid);
        let new_prefix = format!("workbench.panel.aichat.{}.", new_uuid);

        conn.execute(
            "UPDATE ItemTable SET key = REPLACE(key, ?1, ?2) WHERE key LIKE ?3",
            [&old_prefix, &new_prefix, &format!("{}%", old_prefix)],
        )
        .with_context(|| format!("Failed to update UUID: {} -> {}", old_uuid, new_uuid))?;
    }

    Ok(uuid_map.len())
}

#[cfg(test)]
mod tests {
    // Integration tests would require actual project data
}
