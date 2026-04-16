//! Global storage operations
//!
//! Handles updates to ~/Library/Application Support/Cursor/User/globalStorage/storage.json

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::sqlite_value::Utf8SqlValue;

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

/// Update workspace references embedded in global `state.vscdb`.
///
/// Cursor stores several workspace mappings and references in the global database:
/// - `ItemTable.value` / `ItemTable.key`
/// - `cursorDiskKV.key` / `cursorDiskKV.value`
///
/// Updates any exact path/hash strings from old values to new ones, which
/// helps keep move/copy operations from leaving stale workspace IDs behind.
#[allow(clippy::too_many_arguments)]
pub fn update_global_state_db<P: AsRef<Path>>(
    state_db: P,
    old_path: &str,
    new_path: &str,
    old_uri: &str,
    new_uri: &str,
    old_workspace_hash: &str,
    new_workspace_hash: &str,
    dry_run: bool,
) -> Result<bool> {
    let state_db = state_db.as_ref();

    if !state_db.exists() {
        return Ok(false);
    }

    let mut conn = Connection::open(state_db)
        .with_context(|| format!("Failed to open global state DB: {}", state_db.display()))?;

    let mut modified = false;

    // Replace path, URI, and workspace hash references in all known text columns.
    // This is intentionally conservative to avoid schema-specific assumptions and
    // avoids overlap by applying all replacements through placeholders in-memory.
    let replacements = [
        (old_path, new_path),
        (old_uri, new_uri),
        (old_workspace_hash, new_workspace_hash),
    ];
    let normalized_replacements: Vec<(String, String)> = replacements
        .iter()
        .map(|(old, new)| (old.to_string(), new.to_string()))
        .collect();
    let targets = [
        ("ItemTable", "key"),
        ("ItemTable", "value"),
        ("cursorDiskKV", "key"),
        ("cursorDiskKV", "value"),
    ];

    if dry_run {
        for (table, column) in targets {
            if !table_exists(&conn, table)? || !column_exists(&conn, table, column)? {
                continue;
            }

            let query = format!("SELECT {column} FROM {table}");
            let mut stmt = conn.prepare(&query)?;
            let mut rows = stmt.query([])?;

            while let Some(row) = rows.next()? {
                let value = Utf8SqlValue::from_row(row, 0)?;

                let Some(value) = value else {
                    continue;
                };

                if !has_workspace_scoped_reference(value.as_str(), &normalized_replacements) {
                    continue;
                }

                let normalized =
                    normalize_text_replacements(value.as_str(), &normalized_replacements);
                if normalized != value.as_str() {
                    return Ok(true);
                }
            }
        }

        return Ok(false);
    }

    let tx = conn
        .transaction()
        .with_context(|| format!("Failed to start transaction for: {}", state_db.display()))?;

    for (table, column) in targets {
        if !table_exists(&tx, table)? || !column_exists(&tx, table, column)? {
            continue;
        }

        let query = format!("SELECT rowid, {column} FROM {table}");
        let mut stmt = tx.prepare(&query)?;
        let mut rows = stmt.query([])?;
        let mut pending_updates: Vec<(i64, Utf8SqlValue)> = Vec::new();

        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            let value = Utf8SqlValue::from_row(row, 1)?;

            let Some(value) = value else {
                continue;
            };

            if !has_workspace_scoped_reference(value.as_str(), &normalized_replacements) {
                continue;
            }

            let normalized = normalize_text_replacements(value.as_str(), &normalized_replacements);

            if normalized == value.as_str() {
                continue;
            }

            let normalized = match value {
                Utf8SqlValue::Text(_) => Utf8SqlValue::Text(normalized),
                Utf8SqlValue::Blob(_) => Utf8SqlValue::Blob(normalized.into_bytes()),
            };
            pending_updates.push((rowid, normalized));
        }

        for (rowid, normalized) in pending_updates {
            normalized
                .write_back(
                    &tx,
                    &format!("UPDATE {table} SET {column} = ?1 WHERE rowid = ?2"),
                    rowid,
                )
                .with_context(|| {
                    format!("Failed to update {table}.{column} for row {rowid} in global state DB")
                })?;
            modified = true;
        }
    }

    tx.commit()
        .context("Failed to commit global state DB update")?;

    Ok(modified)
}

fn has_workspace_scoped_reference(value: &str, replacements: &[(String, String)]) -> bool {
    let candidates: Vec<&str> = replacements
        .iter()
        .filter_map(|(old, new)| (old != new).then_some(old.as_str()))
        .collect();

    candidates
        .iter()
        .any(|old| has_safe_suffix_match(value, old))
}

fn has_safe_suffix_match(value: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }

    let mut offset = 0usize;
    while let Some(pos) = value[offset..].find(pattern) {
        let absolute_pos = offset + pos;
        let suffix = value[absolute_pos + pattern.len()..].chars().next();

        if is_workspace_value_suffix_terminator(suffix) {
            return true;
        }

        offset = absolute_pos + 1;
    }

    false
}

fn is_workspace_value_suffix_terminator(suffix: Option<char>) -> bool {
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

fn normalize_text_replacements(value: &str, replacements: &[(String, String)]) -> String {
    let normalized_replacements: Vec<(&str, &str)> = replacements
        .iter()
        .filter_map(|(old, new)| (old != new).then_some((old.as_str(), new.as_str())))
        .collect();

    if normalized_replacements.is_empty() {
        return value.to_string();
    }

    let mut token_seed = 0usize;
    let mut tokens = Vec::new();

    let mut staged = value.to_string();
    for (old, _) in &normalized_replacements {
        let mut token = format!("__CURSOR_HELPER_REPLACE_TOKEN_{token_seed}__");
        while staged.contains(&token) || tokens.iter().any(|(old_token, _)| old_token == &token) {
            token_seed += 1;
            token = format!("__CURSOR_HELPER_REPLACE_TOKEN_{token_seed}__");
        }

        staged = replace_workspace_scoped_matches(&staged, old, &token);
        tokens.push((token, *old));
        token_seed += 1;
    }

    let mut normalized = staged;
    for (token, old_value) in tokens {
        if let Some((_, new_value)) = normalized_replacements
            .iter()
            .find(|(old, _)| old == &old_value)
        {
            normalized = normalized.replace(&token, new_value);
        }
    }

    normalized
}

fn replace_workspace_scoped_matches(value: &str, pattern: &str, replacement: &str) -> String {
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
        if is_workspace_value_suffix_terminator(suffix) {
            normalized.push_str(replacement);
        } else {
            normalized.push_str(pattern);
        }

        offset = next_offset;
    }

    normalized.push_str(&value[offset..]);
    normalized
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
            params![table],
            |row| row.get(0),
        )
        .context("Failed to query sqlite_master")?;

    Ok(count > 0)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let query = format!("PRAGMA table_info({})", table);
    let mut stmt = conn
        .prepare(&query)
        .with_context(|| format!("Failed to read schema for table: {table}"))?;

    let mut rows = stmt
        .query([])
        .with_context(|| format!("Failed to query columns for {table}"))?;
    while let Some(row) = rows.next().context("Failed to iterate table info")? {
        let col_name: String = row.get(1)?;
        if col_name == column {
            return Ok(true);
        }
    }

    Ok(false)
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
    use crate::cursor::sqlite_value::Utf8SqlValue;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use tempfile::TempDir;

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

    #[test]
    fn test_update_global_state_db() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("state.vscdb");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            (
                "workspace.key.file:///old/path",
                "meta:file:///old/path/hash-old",
            ),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            ("composer:hash-old", "file:///old/path"),
        )
        .unwrap();
        drop(conn);

        let modified = update_global_state_db(
            &db_path,
            "file:///old/path",
            "file:///new/path",
            "file:///old/path",
            "file:///new/path",
            "hash-old",
            "hash-new",
            false,
        )
        .unwrap();

        assert!(modified);

        let conn = Connection::open(&db_path).unwrap();
        let item_key: String = conn
            .query_row(
                "SELECT key FROM ItemTable WHERE value LIKE '%new/path%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let item_value: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key LIKE '%new/path%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let disk_key: String = conn
            .query_row(
                "SELECT key FROM cursorDiskKV WHERE value LIKE '%new/path%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let disk_value: String = conn
            .query_row(
                "SELECT value FROM cursorDiskKV WHERE key LIKE '%hash-new%'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(item_key, "workspace.key.file:///new/path");
        assert_eq!(item_value, "meta:file:///new/path/hash-new");
        assert_eq!(disk_key, "composer:hash-new");
        assert_eq!(disk_value, "file:///new/path");
    }

    #[test]
    fn test_update_global_state_db_special_chars() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("state-special.vscdb");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            (
                "workspace.key.file:///old%20path%2Fwith%25percent/hash_old",
                "meta:file:///old%20path%2Fwith%25percent",
            ),
        )
        .unwrap();
        drop(conn);

        let modified = update_global_state_db(
            &db_path,
            "file:///old%20path%2Fwith%25percent",
            "file:///new%20path%2Fwith%25percent",
            "file:///old%20path%2Fwith%25percent",
            "file:///new%20path%2Fwith%25percent",
            "hash_old",
            "hash-new",
            false,
        )
        .unwrap();

        assert!(modified);

        let conn = Connection::open(&db_path).unwrap();
        let item: (String, String) = conn
            .query_row(
                "SELECT key, value FROM ItemTable WHERE value LIKE 'meta:file:///new%';",
                [],
                |row| Ok((row.get(0).unwrap(), row.get(1).unwrap())),
            )
            .unwrap();

        assert_eq!(
            item.0,
            "workspace.key.file:///new%20path%2Fwith%25percent/hash-new"
        );
        assert_eq!(item.1, "meta:file:///new%20path%2Fwith%25percent");
    }

    #[test]
    fn test_update_global_state_db_path_then_uri_update_is_safe() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("state-order.vscdb");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            (
                "workspace.key.file:///home/user/project",
                "file:///home/user/project/hash-old",
            ),
        )
        .unwrap();
        drop(conn);

        let modified = update_global_state_db(
            &db_path,
            "/home/user/project",
            "/home/user/project-copy",
            "file:///home/user/project",
            "file:///home/user/project-copy",
            "hash-old",
            "hash-new",
            false,
        )
        .unwrap();
        assert!(modified);

        let conn = Connection::open(&db_path).unwrap();
        let item_key: String = conn
            .query_row("SELECT key FROM ItemTable", [], |row| row.get(0))
            .unwrap();
        let item_value: String = conn
            .query_row("SELECT value FROM ItemTable", [], |row| row.get(0))
            .unwrap();

        assert_eq!(item_key, "workspace.key.file:///home/user/project-copy");
        assert_eq!(item_value, "file:///home/user/project-copy/hash-new");
        assert!(!item_value.contains("project-copy-copy"));
        assert!(!item_key.contains("project-copy-copy"));
        assert!(modified);
    }

    #[test]
    fn test_update_global_state_db_does_not_modify_workspace_prefix_matches() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("state-prefix.vscdb");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            (
                "workspace.key.file:///home/user/project",
                "meta:file:///home/user/project/hash-old",
            ),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            (
                "workspace.key.file:///home/user/projects/foo",
                "meta:file:///home/user/projects/foo/hash-other",
            ),
        )
        .unwrap();
        drop(conn);

        let modified = update_global_state_db(
            &db_path,
            "/home/user/project",
            "/home/user/project-copy",
            "file:///home/user/project",
            "file:///home/user/project-copy",
            "hash-old",
            "hash-new",
            false,
        )
        .unwrap();
        assert!(modified);

        let conn = Connection::open(&db_path).unwrap();
        let updated_value: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key = 'workspace.key.file:///home/user/project-copy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let untouched_value: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key = 'workspace.key.file:///home/user/projects/foo'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(
            updated_value,
            "meta:file:///home/user/project-copy/hash-new"
        );
        assert_eq!(
            untouched_value,
            "meta:file:///home/user/projects/foo/hash-other"
        );
    }

    #[test]
    fn test_update_global_state_db_does_not_corrupt_prefix_values_in_same_cell() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("state-prefix-cell.vscdb");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            (
                "workspace.key.file:///home/user/project",
                r#"{"active":"file:///home/user/project","other":"file:///home/user/projects/foo","cache":"hash-old"}"#,
            ),
        )
        .unwrap();
        drop(conn);

        let modified = update_global_state_db(
            &db_path,
            "/home/user/project",
            "/home/user/project-copy",
            "file:///home/user/project",
            "file:///home/user/project-copy",
            "hash-old",
            "hash-new",
            false,
        )
        .unwrap();
        assert!(modified);

        let conn = Connection::open(&db_path).unwrap();
        let row_key: String = conn
            .query_row("SELECT key FROM ItemTable", [], |row| row.get(0))
            .unwrap();
        let row_value: String = conn
            .query_row("SELECT value FROM ItemTable", [], |row| row.get(0))
            .unwrap();

        assert_eq!(row_key, "workspace.key.file:///home/user/project-copy");
        assert_eq!(
            row_value,
            r#"{"active":"file:///home/user/project-copy","other":"file:///home/user/projects/foo","cache":"hash-new"}"#
        );
        assert!(!row_value.contains("project-copy-copy"));
    }

    #[test]
    fn test_update_global_state_db_updates_utf8_blob_values_without_changing_type() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("state-blob.vscdb");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE cursorDiskKV (key BLOB PRIMARY KEY, value BLOB)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, ?2)",
            (
                "workspace.key.file:///old/path",
                Vec::from("meta:file:///old/path/hash-old".as_bytes()),
            ),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            (
                Vec::from("composer:hash-old".as_bytes()),
                Vec::from("file:///old/path".as_bytes()),
            ),
        )
        .unwrap();
        drop(conn);

        let modified = update_global_state_db(
            &db_path,
            "/old/path",
            "/new/path",
            "file:///old/path",
            "file:///new/path",
            "hash-old",
            "hash-new",
            false,
        )
        .unwrap();

        assert!(modified);

        let conn = Connection::open(&db_path).unwrap();
        let item_value_type: String = conn
            .query_row("SELECT typeof(value) FROM ItemTable", [], |row| row.get(0))
            .unwrap();
        let disk_key_type: String = conn
            .query_row("SELECT typeof(key) FROM cursorDiskKV", [], |row| row.get(0))
            .unwrap();
        let disk_value_type: String = conn
            .query_row("SELECT typeof(value) FROM cursorDiskKV", [], |row| {
                row.get(0)
            })
            .unwrap();
        let item_value = conn
            .query_row("SELECT value FROM ItemTable", [], |row| {
                Utf8SqlValue::from_row(row, 0)
            })
            .unwrap()
            .unwrap();
        let disk_key = conn
            .query_row("SELECT key FROM cursorDiskKV", [], |row| {
                Utf8SqlValue::from_row(row, 0)
            })
            .unwrap()
            .unwrap();
        let disk_value = conn
            .query_row("SELECT value FROM cursorDiskKV", [], |row| {
                Utf8SqlValue::from_row(row, 0)
            })
            .unwrap()
            .unwrap();

        assert_eq!(item_value_type, "blob");
        assert_eq!(disk_key_type, "blob");
        assert_eq!(disk_value_type, "blob");
        assert_eq!(item_value.as_str(), "meta:file:///new/path/hash-new");
        assert_eq!(disk_key.as_str(), "composer:hash-new");
        assert_eq!(disk_value.as_str(), "file:///new/path");
    }

    #[test]
    fn test_update_global_state_db_skips_invalid_utf8_blob_values() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("state-invalid-blob.vscdb");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable(key, value) VALUES (?1, X'80')",
            ["workspace.key.file:///other/path"],
        )
        .unwrap();
        drop(conn);

        let modified = update_global_state_db(
            &db_path,
            "/old/path",
            "/new/path",
            "file:///old/path",
            "file:///new/path",
            "hash-old",
            "hash-new",
            false,
        )
        .unwrap();

        assert!(!modified);

        let conn = Connection::open(&db_path).unwrap();
        let raw: Vec<u8> = conn
            .query_row("SELECT value FROM ItemTable", [], |row| row.get(0))
            .unwrap();
        assert_eq!(raw, vec![0x80]);
    }
}
