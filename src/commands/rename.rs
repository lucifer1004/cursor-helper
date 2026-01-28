//! Rename command implementation
//!
//! Renames or copies a Cursor project while preserving:
//! - Chat history
//! - Workspace settings
//! - MCP cache
//! - Terminal info

use anyhow::{bail, Context, Result};
use fs_extra::dir::{self, CopyOptions};
use owo_colors::OwoColorize;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use url::Url;

use crate::config;
use crate::cursor::{folder_id, storage, workspace};

/// Execute the rename command
pub fn execute(old_path: &str, new_path: &str, dry_run: bool, copy_mode: bool) -> Result<()> {
    // Normalize and validate paths
    let old_path = normalize_path(old_path)?;
    let new_path = normalize_new_path(new_path)?;

    // Validate
    if !old_path.exists() {
        bail!("Old path does not exist: {}", old_path.display());
    }
    if new_path.exists() {
        bail!("New path already exists: {}", new_path.display());
    }

    // Check if Cursor is running (skip in dry-run)
    if !dry_run && is_cursor_running() {
        bail!("Cursor is running. Please close it completely before running this script.");
    }

    // Try to find existing workspace storage
    // This handles symlink issues (e.g., /tmp vs /private/tmp on macOS)
    let (cursor_old_path, old_workspace_hash) = match find_existing_workspace(&old_path)? {
        Some((cursor_path, hash)) => {
            if cursor_path != old_path.to_string_lossy() {
                println!(
                    "{} Cursor recorded path as: {}",
                    "Note:".yellow(),
                    cursor_path
                );
            }
            (PathBuf::from(&cursor_path), hash)
        }
        None => {
            // No existing workspace found, compute from user path
            let hash = workspace::compute_workspace_hash(&old_path)?;
            (old_path.clone(), hash)
        }
    };

    // Compute folder ID from the path Cursor actually used
    let old_folder_id = folder_id::path_to_folder_id(&cursor_old_path);

    // Get directories
    let cursor_projects_dir = config::cursor_projects_dir()?;
    let workspace_storage_dir = config::workspace_storage_dir()?;
    let global_storage_dir = config::global_storage_dir()?;

    let old_projects_dir = cursor_projects_dir.join(&old_folder_id);
    let old_workspace_dir = workspace_storage_dir.join(&old_workspace_hash);

    // Mode description
    let mode = if copy_mode { "COPY" } else { "MOVE" };
    let action = if copy_mode { "Copying" } else { "Moving" };

    // Print summary
    println!();
    println!(
        "{}",
        format!("=== Cursor Project {} Tool ===", mode).green()
    );
    println!();
    println!("Old path: {}", old_path.display());
    println!("New path: {}", new_path.display());
    println!("Mode: {}", mode);
    println!();
    println!("Old folder ID: {}", old_folder_id);
    println!("Old workspace hash: {}", old_workspace_hash);
    println!();

    // Check if old data exists
    print_exists_status("Cursor projects dir", &old_projects_dir);
    print_exists_status("Workspace storage dir", &old_workspace_dir);
    println!();

    // Confirm (skip in dry-run)
    if !dry_run {
        print!("Proceed with {}? (y/N) ", mode.to_lowercase());
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Step 1: Copy/Move the project folder
    println!(
        "{}",
        format!("Step 1: {} project folder...", action).green()
    );
    println!("  {} -> {}", old_path.display(), new_path.display());
    copy_or_move(&old_path, &new_path, copy_mode, dry_run)?;

    // Compute new identifiers
    let new_folder_id = folder_id::path_to_folder_id(&new_path);
    let new_workspace_hash = if dry_run {
        if copy_mode {
            // In copy mode with dry-run, we can't compute the hash
            // because the new folder doesn't exist yet
            println!(
                "  {}",
                "(New folder would get new birthtime, hash computed at runtime)".yellow()
            );
            "<computed-at-runtime>".to_string()
        } else {
            // Move preserves birthtime, estimate hash with new path
            estimate_hash_after_move(&old_path, &new_path)?
        }
    } else {
        workspace::compute_workspace_hash(&new_path)?
    };

    println!("New folder ID: {}", new_folder_id);
    println!("New workspace hash: {}", new_workspace_hash);

    // Step 2: Copy/Move ~/.cursor/projects/
    let new_projects_dir = cursor_projects_dir.join(&new_folder_id);
    if old_projects_dir.exists() {
        println!(
            "{}",
            format!("Step 2: {} cursor projects data...", action).green()
        );
        println!(
            "  {} -> {}",
            old_projects_dir.display(),
            new_projects_dir.display()
        );
        copy_or_move(&old_projects_dir, &new_projects_dir, copy_mode, dry_run)?;
        println!("  -> {}", new_projects_dir.display());
    } else {
        println!("{}", "Step 2: No cursor projects data to migrate".yellow());
    }

    // Step 3: Copy/Move workspaceStorage
    let new_workspace_dir = workspace_storage_dir.join(&new_workspace_hash);
    if old_workspace_dir.exists() {
        println!(
            "{}",
            format!("Step 3: {} workspaceStorage...", action).green()
        );
        println!(
            "  {} -> {}",
            old_workspace_dir.display(),
            new_workspace_dir.display()
        );
        copy_or_move(&old_workspace_dir, &new_workspace_dir, copy_mode, dry_run)?;
        println!("  -> {}", new_workspace_dir.display());

        // Step 4: Update workspace.json
        let workspace_json_path = new_workspace_dir.join("workspace.json");
        println!("{}", "Step 4: Updating workspace.json...".green());

        let new_uri = path_to_file_uri(&new_path)?;

        if dry_run {
            println!(
                "  {} Write to {}:",
                "[DRY-RUN]".blue(),
                workspace_json_path.display()
            );
            println!("  {} folder: {}", "[DRY-RUN]".blue(), new_uri);
        } else {
            let ws = workspace::WorkspaceJson::new(&new_path)?;
            ws.write(&workspace_json_path)?;
        }
        println!("  -> folder URI: {}", new_uri);
    } else {
        println!("{}", "Step 3: No workspaceStorage data to migrate".yellow());
    }

    // Step 5: Update storage.json
    let storage_json_path = global_storage_dir.join("storage.json");
    if storage_json_path.exists() {
        println!(
            "{}",
            "Step 5: Updating globalStorage/storage.json...".green()
        );

        let old_uri = path_to_file_uri(&old_path)?;
        let new_uri = path_to_file_uri(&new_path)?;

        if dry_run {
            println!("  {} Update {} -> {}", "[DRY-RUN]".blue(), old_uri, new_uri);
        }

        let modified =
            storage::update_storage_json(&storage_json_path, &old_uri, &new_uri, dry_run)?;

        if modified {
            println!("  -> Updated workspace references");
        } else {
            println!("  -> No matching references found");
        }
    }

    // Done!
    println!();
    println!("{}", format!("=== {} complete! ===", mode).green());
    println!();

    if dry_run {
        println!("This was a dry-run. No changes were made.");
        println!("Run without --dry-run to apply changes.");
    } else {
        println!("You can now open {} in Cursor.", new_path.display());
        println!("Your chat history and workspace settings should be preserved.");
        if copy_mode {
            println!();
            println!(
                "Original project at {} was kept intact.",
                old_path.display()
            );
        }
    }

    Ok(())
}

/// Find existing workspace storage for a path
/// Returns (cursor_path, workspace_hash) where cursor_path is what Cursor recorded
fn find_existing_workspace(path: &Path) -> Result<Option<(String, String)>> {
    let workspace_storage_dir = config::workspace_storage_dir()?;

    if !workspace_storage_dir.exists() {
        return Ok(None);
    }

    let path_uri = Url::from_file_path(path)
        .map_err(|_| anyhow::anyhow!("Invalid path"))?
        .to_string();
    let path_uri_normalized = path_uri.trim_end_matches('/');

    let result = fs::read_dir(&workspace_storage_dir)?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let workspace_json = entry.path().join("workspace.json");
            let content = fs::read_to_string(&workspace_json).ok()?;
            let ws: workspace::WorkspaceJson = serde_json::from_str(&content).ok()?;

            let folder_uri_normalized = ws.folder.trim_end_matches('/');
            (folder_uri_normalized == path_uri_normalized).then(|| {
                let hash = entry.file_name().to_string_lossy().to_string();
                let cursor_path = Url::parse(&ws.folder)
                    .ok()
                    .and_then(|url| url.to_file_path().ok())
                    .map(|p| p.to_string_lossy().to_string())?;
                Some((cursor_path, hash))
            })?
        });

    Ok(result)
}

/// Normalize an existing path - make absolute and resolve . and .. but NOT symlinks
/// Cursor uses paths as-is (without symlink resolution)
fn normalize_path(path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path);

    if !path.exists() {
        bail!("Path does not exist: {}", path.display());
    }

    // Make absolute and clean . and .. without following symlinks
    let abs_path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(&path)
    };

    Ok(clean_path(&abs_path))
}

/// Normalize a new path (parent must exist)
/// Resolves . and .. WITHOUT following symlinks
fn normalize_new_path(path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path);
    let name = path.file_name().context("New path has no file name")?;

    // Get parent, treating empty parent as current directory
    let parent = path.parent().context("New path has no parent directory")?;
    let parent = if parent.as_os_str().is_empty() {
        std::env::current_dir()?
    } else if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        std::env::current_dir()?.join(parent)
    };

    if !parent.exists() {
        bail!("Parent directory does not exist: {}", parent.display());
    }

    Ok(clean_path(&parent).join(name))
}

/// Clean a path by resolving . and .. components without following symlinks
fn clean_path(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            _ => result.push(component),
        }
    }
    result
}

/// Check if Cursor is running
fn is_cursor_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        Command::new("pgrep")
            .args(["-x", "Cursor"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("pgrep")
            .args(["-x", "cursor"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq Cursor.exe"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("Cursor.exe"))
            .unwrap_or(false)
    }
}

/// Print whether a path exists
fn print_exists_status(label: &str, path: &Path) {
    if path.exists() {
        println!("{} {}: {}", "Found:".green(), label, path.display());
    } else {
        println!("{} {}: {}", "Not found:".yellow(), label, path.display());
    }
}

/// Copy or move a directory, with optional merge if target exists
fn copy_or_move(src: &Path, dst: &Path, copy_mode: bool, dry_run: bool) -> Result<()> {
    let merge = dst.exists();
    if merge {
        println!("  {} {}", "Target exists:".yellow(), dst.display());

        if dry_run {
            println!(
                "  {} Would prompt for merge confirmation",
                "[DRY-RUN]".blue()
            );
        } else {
            print!("  Merge into existing directory? (y/N) ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            if !input.trim().eq_ignore_ascii_case("y") {
                bail!("Aborted: target directory already exists");
            }
        }
    }

    if dry_run {
        return Ok(());
    }

    let options = CopyOptions::new().copy_inside(true).skip_exist(merge);

    let action = if copy_mode { "copy" } else { "move" };
    let result = if copy_mode {
        dir::copy(src, dst, &options).map(|_| ())
    } else {
        dir::move_dir(src, dst, &options).map(|_| ())
    };

    result.with_context(|| {
        format!(
            "Failed to {} {} to {}",
            action,
            src.display(),
            dst.display()
        )
    })
}

/// Convert a path to a file:// URI
fn path_to_file_uri(path: &Path) -> Result<String> {
    let url = Url::from_file_path(path)
        .map_err(|_| anyhow::anyhow!("Failed to convert path to URI: {}", path.display()))?;
    Ok(url.to_string())
}

/// Estimate workspace hash after a move (uses old birthtime with new path)
fn estimate_hash_after_move(old_path: &Path, new_path: &Path) -> Result<String> {
    use std::time::UNIX_EPOCH;

    let metadata = fs::metadata(old_path)?;
    let created = metadata.created()?;
    let duration = created.duration_since(UNIX_EPOCH)?;
    let birthtime_ms = duration.as_secs_f64() * 1000.0;
    let birthtime_rounded = birthtime_ms.round() as u64;

    let input = format!("{}{}", new_path.to_string_lossy(), birthtime_rounded);
    let hash = md5::compute(input.as_bytes());
    Ok(format!("{:x}", hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_path_basic() {
        let path = PathBuf::from("/home/user/project");
        assert_eq!(clean_path(&path), PathBuf::from("/home/user/project"));
    }

    #[test]
    fn test_clean_path_with_current_dir() {
        // . components should be removed
        let path = PathBuf::from("/home/./user/./project");
        assert_eq!(clean_path(&path), PathBuf::from("/home/user/project"));
    }

    #[test]
    fn test_clean_path_with_parent_dir() {
        // .. should navigate up
        let path = PathBuf::from("/home/user/../admin/project");
        assert_eq!(clean_path(&path), PathBuf::from("/home/admin/project"));
    }

    #[test]
    fn test_clean_path_complex() {
        let path = PathBuf::from("/home/user/./foo/../bar/./baz/../qux");
        assert_eq!(clean_path(&path), PathBuf::from("/home/user/bar/qux"));
    }

    #[cfg(windows)]
    #[test]
    fn test_clean_path_windows() {
        let path = PathBuf::from(r"C:\Users\me\..\admin\project");
        assert_eq!(clean_path(&path), PathBuf::from(r"C:\Users\admin\project"));
    }

    #[cfg(not(windows))]
    #[test]
    fn test_path_to_file_uri_unix() {
        let path = PathBuf::from("/home/user/project");
        let uri = path_to_file_uri(&path).unwrap();
        assert_eq!(uri, "file:///home/user/project");
    }

    #[cfg(not(windows))]
    #[test]
    fn test_path_to_file_uri_with_spaces() {
        let path = PathBuf::from("/home/user/my project");
        let uri = path_to_file_uri(&path).unwrap();
        assert_eq!(uri, "file:///home/user/my%20project");
    }

    #[cfg(windows)]
    #[test]
    fn test_path_to_file_uri_windows() {
        let path = PathBuf::from(r"C:\Users\me\project");
        let uri = path_to_file_uri(&path).unwrap();
        assert!(uri.starts_with("file:///"));
        assert!(uri.contains("Users"));
    }
}
