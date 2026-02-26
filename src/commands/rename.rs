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
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;
use url::Url;

use super::utils;
use crate::config;
use crate::cursor::{folder_id, storage, workspace};

/// Execute the rename command
pub fn execute(
    old_path: &str,
    new_path: &str,
    dry_run: bool,
    copy_mode: bool,
    force_index: bool,
) -> Result<()> {
    // Normalize and validate paths
    let old_path = normalize_path(old_path)?;
    let new_path = normalize_new_path(new_path)?;

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
    let storage_json_path = global_storage_dir.join("storage.json");
    let global_state_db_path = global_storage_dir.join("state.vscdb");

    // Mode description
    let mode = if copy_mode { "COPY" } else { "MOVE" };
    let action = if copy_mode { "Copying" } else { "Moving" };

    // Print summary
    println!("{}");
    println!(
        "{}",
        format!("=== Cursor Project {} Tool ===", mode).green()
    );
    println!("{}");
    println!("Old path: {}", old_path.display());
    println!("New path: {}", new_path.display());
    println!("Mode: {}", mode);
    println!("Force index: {}", if force_index { "on" } else { "off" });
    println!("{}", "");
    println!("Old folder ID: {}", old_folder_id);
    println!("Old workspace hash: {}", old_workspace_hash);
    println!("{}", "");

    // Check if old data exists
    print_exists_status("Cursor projects dir", &old_projects_dir);
    print_exists_status("Workspace storage dir", &old_workspace_dir);
    println!("{}");

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

    // Step 0: Create backups for rollback safety
    if dry_run {
        println!("{}", "Step 0: Skipping backups in dry-run mode.".yellow());
    } else {
        println!("{}", "Step 0: Creating safety backup...".green());
        if let Some(backup_dir) = create_rename_backup(
            &old_projects_dir,
            &old_workspace_dir,
            &storage_json_path,
            &global_state_db_path,
            dry_run,
        )? {
            println!("  Backup created at: {}", backup_dir.display());
        }
    }

    let old_uri = path_to_file_uri(&old_path)?;
    let new_uri = path_to_file_uri(&new_path)?;
    let old_path_raw = old_path.to_string_lossy().to_string();
    let new_path_raw = new_path.to_string_lossy().to_string();

    // Compute new identifiers
    let new_folder_id = folder_id::path_to_folder_id(&new_path);
    let new_workspace_hash = if dry_run {
        if copy_mode {
            println!(
                "  {}",
                "(New folder would get new birthtime, hash computed at runtime)".yellow()
            );
            "<computed-at-runtime>".to_string()
        } else {
            estimate_hash_after_move(&old_path, &new_path)?
        }
    } else {
        workspace::compute_workspace_hash(&new_path)?
    };

    println!("New folder ID: {}", new_folder_id);
    println!("New workspace hash: {}", new_workspace_hash);

    // Step 1: Copy/Move the project folder
    println!(
        "{}",
        format!("Step 1: {} project folder...", action).green()
    );
    println!("  {} -> {}", old_path.display(), new_path.display());
    copy_or_move(&old_path, &new_path, copy_mode, dry_run)?;

    let mut source_composer_db: Option<PathBuf> = None;
    if copy_mode && !dry_run && old_workspace_dir.exists() {
        source_composer_db = Some(old_workspace_dir.join("state.vscdb"));
    }

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

    // Step 5: Ensure composer index in workspace state DB
    if !dry_run && new_workspace_dir.exists() {
        let new_workspace_db = new_workspace_dir.join("state.vscdb");
        if new_workspace_db.exists() {
            println!("{}", "Step 5: Synchronizing composer index...".green());
            let updated = sync_workspace_composer_index(
                source_composer_db.as_deref(),
                &new_workspace_db,
                &old_uri,
                &new_uri,
                &old_path_raw,
                &new_path_raw,
                &old_workspace_hash,
                &new_workspace_hash,
                force_index,
                dry_run,
            )?;
            if updated {
                println!("  -> Composer index synchronized");
            } else {
                println!("  -> Composer index already complete");
            }
        } else {
            println!("  -> No workspace state DB found; skipping composer index sync");
        }
    } else if dry_run {
        println!(
            "{}",
            "Step 5: Composer index sync skipped in dry-run".yellow()
        );
    } else {
        println!(
            "{}",
            "Step 5: No workspace state DB available for sync".yellow()
        );
    }

    // Step 6: Update storage.json
    if storage_json_path.exists() {
        println!(
            "{}",
            "Step 6: Updating globalStorage/storage.json...".green()
        );

        if dry_run {
            println!("  {} Update {} -> {}", "[DRY-RUN]".blue(), old_uri, new_uri);
        }

        let mut modified =
            storage::update_storage_json(&storage_json_path, &old_uri, &new_uri, dry_run)?;

        let hash_modified = old_workspace_hash != new_workspace_hash;
        if hash_modified && !dry_run {
            println!(
                "  Note: storage.json hash migration skipped (format may be unsupported): {} -> {}",
                old_workspace_hash, new_workspace_hash
            );
        } else if dry_run {
            println!(
                "  {} Hash migration in storage.json would run if needed",
                "[DRY-RUN]".blue()
            );
        }

        if old_path_raw != new_path_raw {
            let path_text_modified = storage::update_storage_json(
                &storage_json_path,
                &old_path_raw,
                &new_path_raw,
                dry_run,
            )?;
            modified = modified || path_text_modified;
            if path_text_modified {
                println!("  -> Updated raw path text in storage.json");
            }
        }

        if !modified && !hash_modified {
            println!("  -> No matching storage.json updates applied");
        }
    } else {
        println!("{}", "Step 6: No storage.json file found".yellow());
    }

    // Step 7: Update global state DB
    if global_state_db_path.exists() {
        println!(
            "{}",
            "Step 7: Updating globalStorage/state.vscdb...".green()
        );

        if dry_run {
            println!("  {} Update {} -> {}", "[DRY-RUN]".blue(), old_uri, new_uri);
            println!(
                "  {} Update workspace hash {} -> {}",
                "[DRY-RUN]".blue(),
                old_workspace_hash,
                new_workspace_hash
            );
        }

        let uri_modified = storage::update_global_state_db(
            &global_state_db_path,
            &old_uri,
            &new_uri,
            &old_workspace_hash,
            &new_workspace_hash,
            dry_run,
        )?;
        let path_modified = if old_path_raw != new_path_raw {
            storage::update_global_state_db(
                &global_state_db_path,
                &old_path_raw,
                &new_path_raw,
                &old_workspace_hash,
                &new_workspace_hash,
                dry_run,
            )?
        } else {
            false
        };

        if uri_modified || path_modified {
            println!("  -> Updated global state references");
        } else {
            println!("  -> No matching global state references found");
        }
    } else {
        println!("{}", "Step 7: No global state DB found".yellow());
    }

    // Step 8: Clear stale cache directories
    println!("{}", "Step 8: Clearing stale cache data...".green());
    if !dry_run {
        if let Ok(true) = new_workspace_dir.exists().then_some(true) {
            clear_path(
                &new_workspace_dir.join("anysphere.cursor-retrieval"),
                dry_run,
            )?;
        } else {
            println!("  -> No workspace cache directory for new path");
        }

        for cache_dir in config::cursor_cache_dirs()? {
            if cache_dir.exists() {
                clear_path(&cache_dir, dry_run)?;
            }
        }
    } else {
        println!("  -> Cache clear skipped in dry-run");
    }

    // Done!
    println!("{}");
    println!("{}", format!("=== {} complete! ===", mode).green());
    println!("{}");

    if dry_run {
        println!("This was a dry-run. No changes were made.");
        println!("Run without --dry-run to apply changes.");
    } else {
        println!("You can now open {} in Cursor.", new_path.display());
        println!("Your chat history and workspace settings should be preserved.");
        if copy_mode {
            println!("{}");
            println!(
                "Original project at {} was kept intact.",
                old_path.display()
            );
        }
    }

    Ok(())
}

/// Update composer index in workspace state DB when copied from an existing workspace
fn sync_workspace_composer_index(
    source_db_path: Option<&Path>,
    target_db_path: &Path,
    old_uri: &str,
    new_uri: &str,
    old_path: &str,
    new_path: &str,
    old_workspace_hash: &str,
    new_workspace_hash: &str,
    force_index: bool,
    dry_run: bool,
) -> Result<bool> {
    let target_conn = Connection::open(target_db_path).with_context(|| {
        format!(
            "Failed to open target workspace DB: {}",
            target_db_path.display()
        )
    })?;

    let source_data = if let Some(source_db_path) = source_db_path {
        if source_db_path.exists() {
            let source_conn = Connection::open(source_db_path).with_context(|| {
                format!(
                    "Failed to open source workspace DB: {}",
                    source_db_path.display()
                )
            })?;
            fetch_composer_data(&source_conn)?
        } else {
            None
        }
    } else {
        None
    };

    let target_data = fetch_composer_data(&target_conn)?;

    let target_has_all_composers = target_data
        .as_deref()
        .is_some_and(|data| has_all_composers(data));
    let mut needs_update = force_index || !target_has_all_composers;
    if source_data.is_none() && force_index {
        needs_update = true;
    }

    if !needs_update {
        return Ok(false);
    }

    let mut data_to_write = match source_data {
        Some(data) => data,
        None => target_data.unwrap_or_default(),
    };

    if data_to_write.is_empty() {
        return Ok(false);
    }

    data_to_write = normalize_composer_data(
        &data_to_write,
        old_uri,
        new_uri,
        old_path,
        new_path,
        old_workspace_hash,
        new_workspace_hash,
    );

    if dry_run {
        return Ok(true);
    }

    update_composer_data(&target_conn, &data_to_write)?;
    Ok(true)
}

/// Update composer.composerData in ItemTable
fn update_composer_data(conn: &Connection, data: &str) -> Result<()> {
    let updated = conn
        .execute(
            "UPDATE ItemTable SET value = ?1 WHERE key = 'composer.composerData'",
            [data],
        )
        .with_context(|| "Failed to update composer.composerData")?;

    if updated == 0 {
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES ('composer.composerData', ?1)",
            [data],
        )
        .with_context(|| "Failed to insert composer.composerData")?;
    }

    Ok(())
}

fn fetch_composer_data(conn: &Connection) -> Result<Option<String>> {
    match conn.query_row(
        "SELECT value FROM ItemTable WHERE key = 'composer.composerData'",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context("Failed to query composer.composerData"),
    }
}

fn has_all_composers(data: &str) -> bool {
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|v| v.get("allComposers").cloned())
        .and_then(|v| v.as_array().map(|a| !a.is_empty()))
        .unwrap_or(false)
}

fn normalize_composer_data(
    data: &str,
    old_uri: &str,
    new_uri: &str,
    old_path: &str,
    new_path: &str,
    old_workspace_hash: &str,
    new_workspace_hash: &str,
) -> String {
    data.replace(old_uri, new_uri)
        .replace(old_path, new_path)
        .replace(old_workspace_hash, new_workspace_hash)
}

/// Create backup snapshots before mutating Cursor metadata.
fn create_rename_backup(
    old_projects_dir: &Path,
    old_workspace_dir: &Path,
    storage_json_path: &Path,
    global_state_db_path: &Path,
    dry_run: bool,
) -> Result<Option<PathBuf>> {
    if dry_run {
        return Ok(None);
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let backup_root = std::env::temp_dir().join(format!("cursor-helper-rename-backup-{timestamp}"));
    fs::create_dir_all(&backup_root).with_context(|| {
        format!(
            "Failed to create backup directory: {}",
            backup_root.display()
        )
    })?;

    if old_projects_dir.exists() {
        let backup_projects = backup_root.join("old_projects");
        utils::copy_dir(old_projects_dir, &backup_projects)?;
        println!("  Backup projects: {}", backup_projects.display());
    }

    if old_workspace_dir.exists() {
        let backup_workspace = backup_root.join("old_workspace");
        utils::copy_dir(old_workspace_dir, &backup_workspace)?;
        println!("  Backup workspaceStorage: {}", backup_workspace.display());
    }

    if storage_json_path.exists() {
        let target = backup_root.join("storage.json");
        fs::copy(storage_json_path, &target).with_context(|| {
            format!(
                "Failed to backup {} to {}",
                storage_json_path.display(),
                target.display()
            )
        })?;
        println!("  Backup storage.json: {}", target.display());
    }

    if global_state_db_path.exists() {
        let target = backup_root.join("state.vscdb");
        fs::copy(global_state_db_path, &target).with_context(|| {
            format!(
                "Failed to backup {} to {}",
                global_state_db_path.display(),
                target.display()
            )
        })?;
        println!("  Backup global state DB: {}", target.display());
    }

    Ok(Some(backup_root))
}

/// Clear stale cache files or directories.
fn clear_path(path: &Path, dry_run: bool) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    if dry_run {
        println!(
            "  {} Remove {} (dry-run)",
            "[DRY-RUN]".blue(),
            path.display()
        );
        return Ok(());
    }

    if path.is_dir() {
        fs::remove_dir_all(path)
            .with_context(|| format!("Failed to remove directory: {}", path.display()))?;
    } else {
        fs::remove_file(path)
            .with_context(|| format!("Failed to remove file: {}", path.display()))?;
    }
    println!("  -> Removed: {}", path.display());

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
        let path = PathBuf::from("/home/./user/./project");
        assert_eq!(clean_path(&path), PathBuf::from("/home/user/project"));
    }

    #[test]
    fn test_clean_path_with_parent_dir() {
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
