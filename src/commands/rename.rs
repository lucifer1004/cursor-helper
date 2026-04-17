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
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;
use url::Url;

use super::utils;
use crate::config;
use crate::cursor::sqlite_value::query_optional_utf8_string_like_value;
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
    println!();
    println!(
        "{}",
        format!("=== Cursor Project {} Tool ===", mode).green()
    );
    println!();
    println!("Old path: {}", old_path.display());
    println!("New path: {}", new_path.display());
    println!("Mode: {}", mode);
    println!("Force index: {}", if force_index { "on" } else { "off" });
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
    let estimated_move_hash = if !copy_mode {
        Some(estimate_hash_after_move(&old_path, &new_path)?)
    } else {
        None
    };

    // Compute new folder ID (before creating destination)
    let new_folder_id = folder_id::path_to_folder_id(&new_path);
    println!("New folder ID: {}", new_folder_id);

    // Step 1: Copy/Move the project folder
    println!(
        "{}",
        format!("Step 1: {} project folder...", action).green()
    );
    println!("  {} -> {}", old_path.display(), new_path.display());
    copy_or_move(&old_path, &new_path, copy_mode, dry_run)?;

    // Compute new workspace hash after destination exists
    let new_workspace_hash = if dry_run {
        if copy_mode {
            println!(
                "  {}",
                "(New folder would get new birthtime, hash computed at runtime)".yellow()
            );
            "<computed-at-runtime>".to_string()
        } else {
            estimated_move_hash
                .clone()
                .context("Failed to estimate moved workspace hash")?
        }
    } else {
        workspace::compute_workspace_hash(&new_path)?
    };

    println!("New workspace hash: {}", new_workspace_hash);

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

        let modified =
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

        let global_modified = storage::update_global_state_db(
            &global_state_db_path,
            &old_path_raw,
            &new_path_raw,
            &old_uri,
            &new_uri,
            &old_workspace_hash,
            &new_workspace_hash,
            dry_run,
        )?;

        if global_modified {
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
        if new_workspace_dir.exists() {
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

/// Update composer index in workspace state DB when copied from an existing workspace
#[allow(clippy::too_many_arguments)]
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

    let data_to_write = match source_data {
        Some(data) => data,
        None => target_data.unwrap_or_default(),
    };

    if data_to_write.is_empty() {
        return Ok(false);
    }

    let normalized = normalize_composer_data(
        &data_to_write,
        old_uri,
        new_uri,
        old_path,
        new_path,
        old_workspace_hash,
        new_workspace_hash,
    );

    let needs_update = data_to_write.contains(old_uri)
        || data_to_write.contains(old_path)
        || data_to_write.contains(old_workspace_hash);

    if !needs_update && !force_index {
        return Ok(false);
    }

    if normalized == data_to_write && !force_index {
        return Ok(false);
    }

    if dry_run {
        return Ok(true);
    }

    update_composer_data(&target_conn, &normalized)?;
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
    query_optional_utf8_string_like_value(
        conn,
        "SELECT value FROM ItemTable WHERE key = ?1",
        "composer.composerData",
        "value",
    )
    .context("Failed to query composer.composerData")
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
    let replacements = [
        (old_uri, new_uri),
        (old_path, new_path),
        (old_workspace_hash, new_workspace_hash),
    ];
    normalize_text_replacements(data, &replacements)
}

fn normalize_text_replacements(data: &str, replacements: &[(&str, &str)]) -> String {
    let replacements: Vec<(&str, &str)> = replacements
        .iter()
        .copied()
        .filter(|(old, new)| old != new)
        .collect();

    if replacements.is_empty() {
        return data.to_string();
    }

    let mut seed = 0usize;
    let mut working = data.to_string();
    let mut placeholder_map = Vec::new();

    for (old, _) in &replacements {
        let mut token = format!("__CURSOR_HELPER_REPLACE_TOKEN_{seed}__");
        while working.contains(&token)
            || placeholder_map
                .iter()
                .any(|(placeholder, _)| placeholder == &token)
        {
            seed += 1;
            token = format!("__CURSOR_HELPER_REPLACE_TOKEN_{seed}__");
        }

        working = replace_composer_scoped_matches(&working, old, &token);
        placeholder_map.push((token, *old));
        seed += 1;
    }

    let mut normalized = working;
    for (token, old) in placeholder_map {
        if let Some((_, new)) = replacements
            .iter()
            .find(|(candidate_old, _)| candidate_old == &old)
        {
            normalized = normalized.replace(&token, new);
        }
    }

    normalized
}

fn replace_composer_scoped_matches(value: &str, pattern: &str, replacement: &str) -> String {
    if pattern.is_empty() {
        return value.to_string();
    }

    let mut offset = 0usize;
    let mut normalized = String::with_capacity(value.len());

    while let Some(pos) = value[offset..].find(pattern) {
        let absolute_pos = offset + pos;

        normalized.push_str(&value[offset..absolute_pos]);

        let next_offset = absolute_pos + pattern.len();
        let suffix = value[next_offset..].chars().next();
        if is_composer_value_suffix_terminator(suffix) {
            normalized.push_str(replacement);
        } else {
            normalized.push_str(pattern);
        }

        offset = next_offset;
    }

    normalized.push_str(&value[offset..]);
    normalized
}

fn is_composer_value_suffix_terminator(suffix: Option<char>) -> bool {
    match suffix {
        None => true,
        Some(suffix) => {
            !suffix.is_ascii_alphanumeric()
                && suffix != '_'
                && suffix != '-'
                && suffix != '.'
                && suffix != '%'
        }
    }
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
    let result: Result<()> = if copy_mode {
        dir::copy(src, dst, &options)
            .map(|_| ())
            .map_err(Into::into)
    } else {
        move_or_merge_dir(src, dst, merge)
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

fn move_or_merge_dir(src: &Path, dst: &Path, merge: bool) -> Result<()> {
    ensure_real_directory(src, "Source")?;

    if merge {
        ensure_real_directory(dst, "Target")?;
        return move_dir_merge(src, dst);
    }

    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => {
            move_dir_cross_device(src, dst)
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "Failed to atomically rename {} to {}",
                src.display(),
                dst.display()
            )
        }),
    }
}

fn ensure_real_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("Failed to access {label} directory: {}", path.display()))?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        bail!("{label} must not be a symlink: {}", path.display())
    }

    if file_type.is_dir() {
        return Ok(());
    }

    bail!("{label} is not a directory: {}", path.display())
}

fn move_dir_cross_device(src: &Path, dst: &Path) -> Result<()> {
    move_dir_cross_device_with(src, dst, &mut |_, _| Ok(()))
}

fn move_dir_cross_device_with<F>(src: &Path, dst: &Path, before_copy: &mut F) -> Result<()>
where
    F: FnMut(&Path, &Path) -> Result<()>,
{
    move_dir_cross_device_with_hooks(src, dst, before_copy, &mut |_| Ok(()))
}

fn move_dir_cross_device_with_hooks<F, R>(
    src: &Path,
    dst: &Path,
    before_copy: &mut F,
    before_remove: &mut R,
) -> Result<()>
where
    F: FnMut(&Path, &Path) -> Result<()>,
    R: FnMut(&Path) -> Result<()>,
{
    let parent = dst
        .parent()
        .context("Destination path has no parent directory")?;
    let staging_dir = tempfile::Builder::new()
        .prefix(".cursor-helper-move-")
        .tempdir_in(parent)
        .with_context(|| {
            format!(
                "Failed to create staging directory under {}",
                parent.display()
            )
        })?;
    let staging_path = staging_dir.path().join("payload");

    copy_path_no_follow_symlinks_with(src, &staging_path, before_copy)?;
    if let Err(err) = remove_path_no_follow_symlinks_with(src, before_remove) {
        restore_directory_from_staging(&staging_path, src).with_context(|| {
            format!(
                "Failed to restore source after removal error while moving {} to {}",
                src.display(),
                dst.display()
            )
        })?;
        return Err(err).with_context(|| {
            format!(
                "Failed to remove original directory while moving {} to {}",
                src.display(),
                dst.display()
            )
        });
    }

    if let Err(err) = fs::rename(&staging_path, dst) {
        restore_directory_from_staging(&staging_path, src).with_context(|| {
            format!(
                "Failed to restore source after finalize error while moving {} to {}",
                src.display(),
                dst.display()
            )
        })?;
        return Err(err).with_context(|| {
            format!(
                "Failed to finalize staged move from {} to {}",
                src.display(),
                dst.display()
            )
        });
    }

    Ok(())
}

fn move_dir_merge(src: &Path, dst: &Path) -> Result<()> {
    move_dir_merge_with(src, dst, &mut |_, _| Ok(()))
}

fn move_dir_merge_with<F>(src: &Path, dst: &Path, before_copy: &mut F) -> Result<()>
where
    F: FnMut(&Path, &Path) -> Result<()>,
{
    let mut moved_entries = Vec::new();
    copy_dir_contents_no_follow_symlinks_with(src, dst, &mut moved_entries, before_copy)?;

    moved_entries.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for moved_entry in moved_entries {
        remove_path_no_follow_symlinks(&moved_entry)?;
    }

    prune_empty_dirs(src)
}

fn copy_dir_contents_no_follow_symlinks_with<F>(
    src: &Path,
    dst: &Path,
    moved_entries: &mut Vec<PathBuf>,
    before_copy: &mut F,
) -> Result<()>
where
    F: FnMut(&Path, &Path) -> Result<()>,
{
    let entries = fs::read_dir(src)
        .with_context(|| format!("Failed to read directory: {}", src.display()))?;

    for entry in entries {
        let entry = entry.with_context(|| format!("Failed to read entry in {}", src.display()))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let src_type = fs::symlink_metadata(&src_path)
            .with_context(|| format!("Failed to inspect {}", src_path.display()))?
            .file_type();

        if path_exists_no_follow(&dst_path) {
            let dst_type = fs::symlink_metadata(&dst_path)
                .with_context(|| format!("Failed to inspect {}", dst_path.display()))?
                .file_type();
            if src_type.is_dir() && dst_type.is_dir() {
                copy_dir_contents_no_follow_symlinks_with(
                    &src_path,
                    &dst_path,
                    moved_entries,
                    before_copy,
                )?;
            }
            continue;
        }

        copy_path_no_follow_symlinks_with(&src_path, &dst_path, before_copy)?;
        moved_entries.push(src_path);
    }

    Ok(())
}

fn copy_path_no_follow_symlinks_with<F>(src: &Path, dst: &Path, before_copy: &mut F) -> Result<()>
where
    F: FnMut(&Path, &Path) -> Result<()>,
{
    before_copy(src, dst)?;

    let metadata = fs::symlink_metadata(src)
        .with_context(|| format!("Failed to inspect {}", src.display()))?;
    let file_type = metadata.file_type();

    if file_type.is_dir() {
        fs::create_dir(dst)
            .with_context(|| format!("Failed to create directory: {}", dst.display()))?;

        let entries = fs::read_dir(src)
            .with_context(|| format!("Failed to read directory: {}", src.display()))?;
        for entry in entries {
            let entry =
                entry.with_context(|| format!("Failed to read entry in {}", src.display()))?;
            let child_src = entry.path();
            let child_dst = dst.join(entry.file_name());
            copy_path_no_follow_symlinks_with(&child_src, &child_dst, before_copy)?;
        }

        fs::set_permissions(dst, metadata.permissions())
            .with_context(|| format!("Failed to set permissions on {}", dst.display()))?;
        return Ok(());
    }

    if file_type.is_file() {
        fs::copy(src, dst)
            .with_context(|| format!("Failed to copy {} to {}", src.display(), dst.display()))?;
        fs::set_permissions(dst, metadata.permissions())
            .with_context(|| format!("Failed to set permissions on {}", dst.display()))?;
        return Ok(());
    }

    if file_type.is_symlink() {
        let target = fs::read_link(src)
            .with_context(|| format!("Failed to read symlink target for {}", src.display()))?;
        let is_dir = symlink_targets_directory(src, &file_type);
        create_symlink_at(dst, &target, is_dir).with_context(|| {
            format!(
                "Failed to recreate symlink {} -> {}",
                dst.display(),
                target.display()
            )
        })?;
        return Ok(());
    }

    bail!("Unsupported filesystem entry: {}", src.display())
}

fn remove_path_no_follow_symlinks(path: &Path) -> Result<()> {
    remove_path_no_follow_symlinks_with(path, &mut |_| Ok(()))
}

fn remove_path_no_follow_symlinks_with<F>(path: &Path, before_remove: &mut F) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("Failed to inspect {}", path.display()))?;
    let file_type = metadata.file_type();
    before_remove(path)?;

    if file_type.is_dir() {
        let entries = fs::read_dir(path)
            .with_context(|| format!("Failed to read directory: {}", path.display()))?;
        for entry in entries {
            let entry =
                entry.with_context(|| format!("Failed to read entry in {}", path.display()))?;
            remove_path_no_follow_symlinks_with(&entry.path(), before_remove)?;
        }
        fs::remove_dir(path)
            .with_context(|| format!("Failed to remove directory: {}", path.display()))?;
        return Ok(());
    }

    fs::remove_file(path).with_context(|| format!("Failed to remove file: {}", path.display()))?;
    Ok(())
}

fn restore_directory_from_staging(staging_path: &Path, src: &Path) -> Result<()> {
    let mut noop = |_: &Path, _: &Path| Ok(());

    if path_exists_no_follow(src) {
        ensure_real_directory(src, "Source")?;
        let mut restored_entries = Vec::new();
        copy_dir_contents_no_follow_symlinks_with(
            staging_path,
            src,
            &mut restored_entries,
            &mut noop,
        )?;
    } else {
        copy_path_no_follow_symlinks_with(staging_path, src, &mut noop)?;
    }

    Ok(())
}

fn prune_empty_dirs(path: &Path) -> Result<()> {
    if !path_exists_no_follow(path) {
        return Ok(());
    }

    if !fs::symlink_metadata(path)
        .with_context(|| format!("Failed to inspect {}", path.display()))?
        .file_type()
        .is_dir()
    {
        return Ok(());
    }

    let children: Vec<PathBuf> = fs::read_dir(path)
        .with_context(|| format!("Failed to read directory: {}", path.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("Failed to enumerate directory: {}", path.display()))?;

    for child in &children {
        if fs::symlink_metadata(child)
            .with_context(|| format!("Failed to inspect {}", child.display()))?
            .file_type()
            .is_dir()
        {
            prune_empty_dirs(child)?;
        }
    }

    if fs::read_dir(path)
        .with_context(|| format!("Failed to read directory: {}", path.display()))?
        .next()
        .is_none()
    {
        fs::remove_dir(path)
            .with_context(|| format!("Failed to remove directory: {}", path.display()))?;
    }

    Ok(())
}

fn path_exists_no_follow(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[cfg(windows)]
fn symlink_targets_directory(src: &Path, file_type: &fs::FileType) -> bool {
    use std::os::windows::fs::FileTypeExt;

    if file_type.is_symlink_dir() {
        return true;
    }
    if file_type.is_symlink_file() {
        return false;
    }

    fs::metadata(src).map(|meta| meta.is_dir()).unwrap_or(false)
}

#[cfg(not(windows))]
fn symlink_targets_directory(src: &Path, _file_type: &fs::FileType) -> bool {
    fs::metadata(src).map(|meta| meta.is_dir()).unwrap_or(false)
}

#[cfg(unix)]
fn create_symlink_at(path: &Path, target: &Path, _is_dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

#[cfg(windows)]
fn create_symlink_at(path: &Path, target: &Path, is_dir: bool) -> std::io::Result<()> {
    if is_dir {
        std::os::windows::fs::symlink_dir(target, path)
    } else {
        std::os::windows::fs::symlink_file(target, path)
    }
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
    use rusqlite::Connection;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::{symlink, MetadataExt};

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

    #[cfg(unix)]
    #[test]
    fn test_move_or_merge_dir_uses_atomic_rename_and_preserves_broken_symlink() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("project-old");
        let dst = temp_dir.path().join("project-new");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("app.py"), "print('ok')\n").unwrap();
        symlink(Path::new("/missing/python3.9"), src.join("python")).unwrap();

        let src_inode = fs::metadata(&src).unwrap().ino();
        move_or_merge_dir(&src, &dst, false).unwrap();

        assert!(!src.exists());
        assert_eq!(fs::metadata(&dst).unwrap().ino(), src_inode);
        assert_eq!(
            fs::read_to_string(dst.join("app.py")).unwrap(),
            "print('ok')\n"
        );
        assert!(fs::symlink_metadata(dst.join("python"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_link(dst.join("python")).unwrap(),
            PathBuf::from("/missing/python3.9")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_move_or_merge_dir_rejects_source_symlink_root() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("real-project");
        let src = temp_dir.path().join("project-link");
        let dst = temp_dir.path().join("project-new");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("app.py"), "print('ok')\n").unwrap();
        symlink(&target, &src).unwrap();

        let err = move_or_merge_dir(&src, &dst, false).unwrap_err();

        assert!(err.to_string().contains("must not be a symlink"));
        assert!(src.exists());
        assert!(!dst.exists());
        assert_eq!(
            fs::read_to_string(target.join("app.py")).unwrap(),
            "print('ok')\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_move_dir_cross_device_preserves_broken_symlink() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("project-old");
        let dst = temp_dir.path().join("project-new");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("README.md"), "hello\n").unwrap();
        symlink(Path::new("/missing/python3.9"), src.join("python")).unwrap();

        move_dir_cross_device(&src, &dst).unwrap();

        assert!(!src.exists());
        assert_eq!(
            fs::read_to_string(dst.join("README.md")).unwrap(),
            "hello\n"
        );
        assert!(fs::symlink_metadata(dst.join("python"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_link(dst.join("python")).unwrap(),
            PathBuf::from("/missing/python3.9")
        );
    }

    #[test]
    fn test_move_dir_cross_device_failure_keeps_source_intact() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("project-old");
        let dst = temp_dir.path().join("project-new");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("one.txt"), "one\n").unwrap();
        fs::write(src.join("two.txt"), "two\n").unwrap();

        let mut seen_files = 0usize;
        let err = move_dir_cross_device_with(&src, &dst, &mut |candidate, _| {
            if fs::symlink_metadata(candidate)
                .unwrap()
                .file_type()
                .is_file()
            {
                seen_files += 1;
                if seen_files == 2 {
                    bail!("injected copy failure");
                }
            }
            Ok(())
        })
        .unwrap_err();

        assert!(err.to_string().contains("injected copy failure"));
        assert!(src.exists());
        assert_eq!(fs::read_to_string(src.join("one.txt")).unwrap(), "one\n");
        assert_eq!(fs::read_to_string(src.join("two.txt")).unwrap(), "two\n");
        assert!(!dst.exists());
    }

    #[test]
    fn test_move_dir_cross_device_remove_failure_restores_source() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("project-old");
        let dst = temp_dir.path().join("project-new");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("one.txt"), "one\n").unwrap();
        fs::write(src.join("two.txt"), "two\n").unwrap();

        let mut removed_files = 0usize;
        let err =
            move_dir_cross_device_with_hooks(&src, &dst, &mut |_, _| Ok(()), &mut |candidate| {
                if fs::symlink_metadata(candidate)
                    .unwrap()
                    .file_type()
                    .is_file()
                {
                    removed_files += 1;
                    if removed_files == 2 {
                        bail!("injected remove failure");
                    }
                }
                Ok(())
            })
            .unwrap_err();

        assert_eq!(removed_files, 2);
        assert!(!err.to_string().is_empty());
        assert!(src.exists());
        assert_eq!(fs::read_to_string(src.join("one.txt")).unwrap(), "one\n");
        assert_eq!(fs::read_to_string(src.join("two.txt")).unwrap(), "two\n");
        assert!(!dst.exists());
    }

    #[test]
    fn test_move_dir_merge_skips_conflicts_and_removes_copied_entries() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("project-old");
        let dst = temp_dir.path().join("project-new");
        fs::create_dir_all(src.join("subdir")).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join("conflict.txt"), "from source\n").unwrap();
        fs::write(src.join("subdir").join("moved.txt"), "moved\n").unwrap();
        fs::write(dst.join("conflict.txt"), "from destination\n").unwrap();

        move_dir_merge(&src, &dst).unwrap();

        assert_eq!(
            fs::read_to_string(dst.join("conflict.txt")).unwrap(),
            "from destination\n"
        );
        assert_eq!(
            fs::read_to_string(dst.join("subdir").join("moved.txt")).unwrap(),
            "moved\n"
        );
        assert!(src.join("conflict.txt").exists());
        assert!(!src.join("subdir").exists());
        assert!(src.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_move_dir_merge_rejects_target_symlink_root() {
        let temp_dir = TempDir::new().unwrap();
        let src = temp_dir.path().join("project-old");
        let dst_target = temp_dir.path().join("real-target");
        let dst = temp_dir.path().join("project-link");
        fs::create_dir(&src).unwrap();
        fs::create_dir(&dst_target).unwrap();
        fs::write(src.join("moved.txt"), "moved\n").unwrap();
        symlink(&dst_target, &dst).unwrap();

        let err = move_or_merge_dir(&src, &dst, true).unwrap_err();

        assert!(err.to_string().contains("must not be a symlink"));
        assert!(src.join("moved.txt").exists());
        assert!(fs::read_dir(&dst_target).unwrap().next().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn test_path_to_file_uri_windows() {
        let path = PathBuf::from(r"C:\Users\me\project");
        let uri = path_to_file_uri(&path).unwrap();
        assert!(uri.starts_with("file:///"));
        assert!(uri.contains("Users"));
    }

    #[test]
    fn test_sync_workspace_composer_index_normalizes_when_all_composers_present() {
        let temp_dir = TempDir::new().unwrap();
        let source_db = temp_dir.path().join("source.vscdb");
        let target_db = temp_dir.path().join("target.vscdb");

        let composer_payload = r#"{"allComposers":[{"id":"file:///old/project/hash_old"}]}"#;
        let conn = Connection::open(&source_db).unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            ("composer.composerData", composer_payload),
        )
        .unwrap();
        drop(conn);

        let conn = Connection::open(&target_db).unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            ("composer.composerData", composer_payload),
        )
        .unwrap();
        drop(conn);

        let updated = sync_workspace_composer_index(
            Some(&source_db),
            &target_db,
            "file:///old/project",
            "file:///new/project",
            "/old/project",
            "/new/project",
            "hash_old",
            "hash_new",
            false,
            false,
        )
        .unwrap();

        assert!(updated);

        let conn = Connection::open(&target_db).unwrap();
        let value: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key = 'composer.composerData'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert!(value.contains("file:///new/project"));
        assert!(value.contains("hash_new"));
        assert!(!value.contains("file:///old/project"));
        assert!(!value.contains("hash_old"));
    }

    #[test]
    fn test_sync_workspace_composer_index_no_update_without_stale_references() {
        let temp_dir = TempDir::new().unwrap();
        let target_db = temp_dir.path().join("target.vscdb");

        let composer_payload = r#"{"allComposers":[{"id":"file:///new/project/hash_new"}]}"#;
        let conn = Connection::open(&target_db).unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            ("composer.composerData", composer_payload),
        )
        .unwrap();
        drop(conn);

        let updated = sync_workspace_composer_index(
            None,
            &target_db,
            "file:///old/project",
            "file:///new/project",
            "/old/project",
            "/new/project",
            "hash_old",
            "hash_new",
            false,
            false,
        )
        .unwrap();

        assert!(!updated);
    }

    #[test]
    fn test_sync_workspace_composer_index_force_index_writes_without_stale_references() {
        let temp_dir = TempDir::new().unwrap();
        let target_db = temp_dir.path().join("target.vscdb");

        let composer_payload = r#"{"allComposers":[{"id":"file:///new/project/hash_new"}]}"#;
        let conn = Connection::open(&target_db).unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            ("composer.composerData", composer_payload),
        )
        .unwrap();
        drop(conn);

        let updated = sync_workspace_composer_index(
            None,
            &target_db,
            "file:///old/project",
            "file:///new/project",
            "/old/project",
            "/new/project",
            "hash_old",
            "hash_new",
            true,
            false,
        )
        .unwrap();

        assert!(updated);

        let conn = Connection::open(&target_db).unwrap();
        let value: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key = 'composer.composerData'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, composer_payload);
    }

    #[test]
    fn test_normalize_composer_data_avoids_uri_double_expand() {
        let value = r#"{"allComposers":[{"id":"file:///home/user/project/hash_old","path":"/home/user/project","title":"project"}]}"#;

        let normalized = normalize_composer_data(
            value,
            "file:///home/user/project",
            "file:///home/user/project-copy",
            "/home/user/project",
            "/home/user/project-copy",
            "hash_old",
            "hash_new",
        );

        assert_eq!(
            normalized,
            r#"{"allComposers":[{"id":"file:///home/user/project-copy/hash_new","path":"/home/user/project-copy","title":"project"}]}"#
        );
        assert!(!normalized.contains("file:///home/user/project-copy-copy"));
    }

    #[test]
    fn test_normalize_composer_data_preserves_prefix_sibling_paths_in_same_cell() {
        let value = r#"{"active":"file:///home/user/project","other":"file:///home/user/projects/foo","cache":"hash_old"}"#;

        let normalized = normalize_composer_data(
            value,
            "file:///home/user/project",
            "file:///home/user/project-copy",
            "/home/user/project",
            "/home/user/project-copy",
            "hash_old",
            "hash_new",
        );

        assert_eq!(
            normalized,
            r#"{"active":"file:///home/user/project-copy","other":"file:///home/user/projects/foo","cache":"hash_new"}"#
        );
    }

    #[test]
    fn test_normalize_composer_data_handles_encoded_uri() {
        let value = r#"{"allComposers":[{"id":"file:///home/user/my%20project/hash_old"}]}"#;
        let normalized = normalize_composer_data(
            value,
            "file:///home/user/my%20project",
            "file:///home/user/my%20project-backup",
            "/home/user/my project",
            "/home/user/my project-backup",
            "hash_old",
            "hash_new",
        );

        assert_eq!(
            normalized,
            r#"{"allComposers":[{"id":"file:///home/user/my%20project-backup/hash_new"}]}"#
        );
    }
}
