//! Clean command - Remove orphaned workspace storage

use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use super::utils;
use crate::config;

/// Orphaned workspace entry
#[derive(Debug)]
pub struct OrphanedWorkspace {
    /// Path to the workspace storage directory
    pub storage_path: PathBuf,
    /// The folder URL stored in workspace.json
    pub folder_url: String,
    /// Size in bytes
    pub size_bytes: u64,
}

/// Execute the clean command
pub fn execute(dry_run: bool, yes: bool) -> Result<()> {
    let workspace_storage_dir = config::workspace_storage_dir()
        .context("Failed to determine workspace storage directory")?;

    if !workspace_storage_dir.exists() {
        println!("No workspace storage directory found.");
        return Ok(());
    }

    // Find orphaned workspaces
    let orphaned = find_orphaned_workspaces(&workspace_storage_dir)?;

    if orphaned.is_empty() {
        println!("No orphaned workspaces found. Everything is clean!");
        return Ok(());
    }

    // Calculate total size
    let total_size: u64 = orphaned.iter().map(|o| o.size_bytes).sum();

    println!("Found {} orphaned workspace(s):\n", orphaned.len());

    for entry in &orphaned {
        println!(
            "  {} ({})",
            entry.storage_path.display(),
            utils::format_size(entry.size_bytes)
        );
        println!("    Original: {}", entry.folder_url.dimmed());
    }

    println!(
        "\nTotal: {} in {} item(s)",
        utils::format_size(total_size),
        orphaned.len()
    );

    if dry_run {
        println!("\n{}", "(DRY-RUN) No changes made.".blue());
        println!("Run with --yes to delete these workspaces.");
        return Ok(());
    }

    // Confirm deletion
    if !yes {
        print!("\nDelete these orphaned workspaces? (y/N) ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Delete orphaned workspaces
    let mut deleted = 0;
    let mut failed = 0;

    for entry in &orphaned {
        match fs::remove_dir_all(&entry.storage_path) {
            Ok(_) => {
                println!("{} {}", "Deleted:".green(), entry.storage_path.display());
                deleted += 1;
            }
            Err(e) => {
                eprintln!(
                    "{} {}: {}",
                    "Failed:".red(),
                    entry.storage_path.display(),
                    e
                );
                failed += 1;
            }
        }
    }

    println!(
        "\nCleaned up {} workspace(s), {} failed",
        deleted.to_string().green(),
        if failed > 0 {
            failed.to_string().red().to_string()
        } else {
            "0".to_string()
        }
    );

    Ok(())
}

/// Find workspaces whose project folders no longer exist
fn find_orphaned_workspaces(workspace_storage_dir: &PathBuf) -> Result<Vec<OrphanedWorkspace>> {
    let mut orphaned = Vec::new();

    let entries = fs::read_dir(workspace_storage_dir)
        .with_context(|| format!("Failed to read: {}", workspace_storage_dir.display()))?;

    for entry in entries.flatten() {
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let workspace_dir = entry.path();
        let workspace_json = workspace_dir.join("workspace.json");

        if !workspace_json.exists() {
            continue;
        }

        // Read workspace.json
        let content = match fs::read_to_string(&workspace_json) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let ws: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let folder_url = match ws.get("folder").and_then(|v| v.as_str()) {
            Some(f) => f.to_string(),
            None => continue,
        };

        // Check if the project folder exists
        let is_orphaned = match url::Url::parse(&folder_url) {
            Ok(url) => {
                if url.scheme() == "file" {
                    // Local file - check if it exists
                    match url.to_file_path() {
                        Ok(path) => !path.exists(),
                        Err(_) => false, // Can't parse path, assume not orphaned
                    }
                } else {
                    // Remote workspace (ssh, tunnel, etc.) - can't check, skip
                    false
                }
            }
            Err(_) => false,
        };

        if is_orphaned {
            let size_bytes = utils::calculate_dir_size(&workspace_dir).unwrap_or(0);

            orphaned.push(OrphanedWorkspace {
                storage_path: workspace_dir,
                folder_url,
                size_bytes,
            });
        }
    }

    // Sort by size (largest first)
    orphaned.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));

    Ok(orphaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orphaned_workspace_struct() {
        let orphaned = OrphanedWorkspace {
            storage_path: PathBuf::from("/path/to/storage"),
            folder_url: "file:///old/project".to_string(),
            size_bytes: 1024 * 1024, // 1 MB
        };

        assert_eq!(orphaned.storage_path, PathBuf::from("/path/to/storage"));
        assert_eq!(orphaned.folder_url, "file:///old/project");
        assert_eq!(orphaned.size_bytes, 1024 * 1024);
    }

    #[test]
    fn test_orphaned_workspace_debug() {
        let orphaned = OrphanedWorkspace {
            storage_path: PathBuf::from("/test"),
            folder_url: "file:///test".to_string(),
            size_bytes: 0,
        };

        // Should implement Debug
        let debug_str = format!("{:?}", orphaned);
        assert!(debug_str.contains("OrphanedWorkspace"));
    }
}
