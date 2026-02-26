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
/// This updates any exact path hash strings from old values to new ones, which
/// helps keep move/copy operations from leaving stale workspace IDs behind.
pub fn update_global_state_db<P: AsRef<Path>>(
    state_db: P,
    old_path: &str,
    new_path: &str,
    old_workspace_hash: &str,
    new_workspace_hash: &str,
    dry_run: bool,
) -> Result<bool> {
    let state_db = state_db.as_ref();

    if !state_db.exists() {
        return Ok(false);
    }

    let conn = Connection::open(state_db)
        .with_context(|| format!("Failed to open global state DB: {}", state_db.display()))?;

    let mut modified = false;

    // Replace both path references and workspace hash references in all known text
    // columns. This is intentionally conservative to avoid schema-specific assumptions.
    let replacements = [
        (old_path, new_path),
        (old_workspace_hash, new_workspace_hash),
    ];
    let targets = [
        ("ItemTable", "key"),
        ("ItemTable", "value"),
        ("cursorDiskKV", "key"),
        ("cursorDiskKV", "value"),
    ];

    for (table, column) in targets {
        if !table_exists(&conn, table)? || !column_exists(&conn, table, column)? {
            continue;
        }

        for (old, new) in replacements {
            if old == new {
                continue;
            }

            let affected = count_rows_with_reference(&conn, table, column, old)?;
            if affected > 0 {
                modified = true;
            }

            if !dry_run {
                replace_in_column(&conn, table, column, old, new)?;
            }
        }
    }

    Ok(modified)
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

fn count_rows_with_reference(
    conn: &Connection,
    table: &str,
    column: &str,
    needle: &str,
) -> Result<i64> {
    let query = format!("SELECT COUNT(*) FROM {table} WHERE {column} LIKE '%' || ?1 || '%'");
    conn.query_row(&query, params![needle], |row| row.get(0))
        .with_context(|| format!("Failed to count references in {table}.{column}"))
}

fn replace_in_column(
    conn: &Connection,
    table: &str,
    column: &str,
    old: &str,
    new: &str,
) -> Result<usize> {
    let query = format!(
        "UPDATE {table} SET {column} = REPLACE({column}, ?1, ?2) WHERE {column} LIKE '%' || ?1 || '%'"
    );

    let replaced = conn
        .execute(&query, params![old, new])
        .with_context(|| format!("Failed to replace references in {table}.{column}"))?;

    Ok(replaced)
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
}
