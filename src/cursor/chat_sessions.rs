//! Shared chat session discovery across Cursor storage layouts.

use anyhow::{Context, Result};
use percent_encoding::percent_decode_str;
use rusqlite::Connection;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::cursor::sqlite_value::query_optional_utf8_string_like_value;
use crate::cursor::workspace;

const GLOBAL_HEADERS_KEY: &str = "composer.composerHeaders";
const LOCAL_COMPOSER_DATA_KEY: &str = "composer.composerData";

/// Stable session metadata used by list, stats, and export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMetadata {
    pub composer_id: String,
    pub title: Option<String>,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Default)]
struct WorkspaceIdentity {
    workspace_id: Option<String>,
    folder_uri_normalized: Option<String>,
    workspace_path_normalized: Option<String>,
    remote_authority: Option<String>,
    is_remote: bool,
}

impl WorkspaceIdentity {
    fn from_workspace_dir(workspace_dir: &Path) -> Self {
        let workspace_id = workspace_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string());

        let workspace_json_path = workspace_dir.join("workspace.json");
        if !workspace_json_path.exists() {
            return Self {
                workspace_id,
                ..Self::default()
            };
        }

        let folder_uri = workspace::read_workspace_target_uri(workspace_dir)
            .ok()
            .flatten();

        let folder_uri_normalized = folder_uri.as_deref().map(normalize_uri_for_comparison);
        let workspace_path_normalized = folder_uri.as_deref().and_then(extract_workspace_path);
        let remote_authority = folder_uri.as_deref().and_then(extract_uri_authority);
        let is_remote = folder_uri
            .as_deref()
            .and_then(uri_scheme)
            .is_some_and(|scheme| scheme == "vscode-remote");

        Self {
            workspace_id,
            folder_uri_normalized,
            workspace_path_normalized,
            remote_authority,
            is_remote,
        }
    }
}

/// Discover stable, top-level exportable sessions for a workspace.
pub fn discover_workspace_sessions(
    workspace_dir: &Path,
    include_archived: bool,
) -> Result<Vec<SessionMetadata>> {
    let identity = WorkspaceIdentity::from_workspace_dir(workspace_dir);

    let mut global_open_error = None;
    let global_conn = match open_global_state_db() {
        Ok(conn) => conn,
        Err(err) => {
            global_open_error = Some(err);
            None
        }
    };

    let mut sessions = Vec::new();
    let mut local_registry_checked = false;
    let mut local_registry_present = false;

    if let Some(conn) = global_conn.as_ref() {
        match load_global_registry_sessions(conn, &identity, include_archived) {
            Ok(discovered) => sessions.extend(discovered),
            Err(err) => {
                global_open_error = Some(err);
            }
        }
    }

    let local_conn = match open_workspace_state_db(workspace_dir) {
        Ok(conn) => conn,
        Err(err) => {
            if sessions.is_empty() {
                if let Some(global_err) = global_open_error {
                    return Err(global_err).context(err.to_string());
                }
                return Err(err);
            }
            None
        }
    };
    if let Some(conn) = local_conn.as_ref() {
        local_registry_checked = true;
        match load_legacy_local_sessions(conn, include_archived) {
            Ok(discovered) => {
                local_registry_present = local_registry_shape_present(conn).unwrap_or(false);
                sessions.extend(discovered);
            }
            Err(err) => {
                if sessions.is_empty() {
                    if let Some(global_err) = global_open_error {
                        return Err(global_err).context(err.to_string());
                    }
                    return Err(err);
                }
            }
        }
    }

    if sessions.is_empty() {
        if local_registry_checked && local_registry_present {
            return Ok(vec![]);
        }

        if let Some(err) = global_open_error {
            return Err(err);
        }

        return Ok(vec![]);
    }

    dedupe_sessions(&mut sessions);

    exclude_child_sessions_from_sources(global_conn.as_ref(), local_conn.as_ref(), &mut sessions);

    sort_sessions(&mut sessions);

    Ok(sessions)
}

/// Count stable, top-level exportable sessions for a workspace.
pub fn count_workspace_sessions(workspace_dir: &Path, include_archived: bool) -> Result<usize> {
    Ok(discover_workspace_sessions(workspace_dir, include_archived)?.len())
}

/// Count sessions, but treat a missing or unreadable global registry as "unknown"
/// when there is no matching local workspace session data to inspect.
pub fn count_workspace_sessions_if_available(
    workspace_dir: &Path,
    include_archived: bool,
) -> Result<Option<usize>> {
    match count_workspace_sessions(workspace_dir, include_archived) {
        Ok(count) => Ok(Some(count)),
        Err(err) if local_session_registry_shape_known(workspace_dir)? => Err(err),
        Err(_) => Ok(None),
    }
}

/// Open Cursor's global `state.vscdb` if it exists.
pub fn open_global_state_db() -> Result<Option<Connection>> {
    let Some(db_path) = global_state_db_path()? else {
        return Ok(None);
    };

    Ok(Some(open_read_only_db(&db_path)?))
}

fn global_state_db_path() -> Result<Option<PathBuf>> {
    let db_path = crate::config::global_storage_dir()?.join("state.vscdb");
    Ok(db_path.exists().then_some(db_path))
}

fn open_workspace_state_db(workspace_dir: &Path) -> Result<Option<Connection>> {
    let db_path = workspace_dir.join("state.vscdb");
    if !db_path.exists() {
        return Ok(None);
    }

    Ok(Some(open_read_only_db(&db_path)?))
}

fn local_session_registry_shape_known(workspace_dir: &Path) -> Result<bool> {
    let Some(conn) = open_workspace_state_db(workspace_dir)? else {
        return Ok(false);
    };

    local_registry_shape_present(&conn)
}

fn local_registry_shape_present(conn: &Connection) -> Result<bool> {
    let Some(data) = query_item_table_value(conn, LOCAL_COMPOSER_DATA_KEY)? else {
        return Ok(false);
    };

    let json: Value =
        serde_json::from_str(&data).context("Failed to parse workspace composer data")?;
    Ok(json
        .get("allComposers")
        .and_then(|value| value.as_array())
        .is_some())
}

fn open_read_only_db(db_path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("Failed to open database: {}", db_path.display()))
}

fn load_global_registry_sessions(
    conn: &Connection,
    identity: &WorkspaceIdentity,
    include_archived: bool,
) -> Result<Vec<SessionMetadata>> {
    let Some(data) = query_item_table_value(conn, GLOBAL_HEADERS_KEY)? else {
        return Ok(vec![]);
    };

    parse_global_registry(&data, identity, include_archived)
}

fn load_legacy_local_sessions(
    conn: &Connection,
    include_archived: bool,
) -> Result<Vec<SessionMetadata>> {
    let Some(data) = query_item_table_value(conn, LOCAL_COMPOSER_DATA_KEY)? else {
        return Ok(vec![]);
    };

    parse_local_registry(&data, include_archived)
}

fn query_item_table_value(conn: &Connection, key: &str) -> Result<Option<String>> {
    query_optional_utf8_string_like_value(
        conn,
        "SELECT value FROM ItemTable WHERE key = ?1",
        key,
        "value",
    )
    .with_context(|| format!("Failed to query ItemTable for key: {}", key))
}

fn query_cursor_disk_value(conn: &Connection, key: &str) -> Result<Option<String>> {
    query_optional_utf8_string_like_value(
        conn,
        "SELECT value FROM cursorDiskKV WHERE key = ?1",
        key,
        "value",
    )
    .with_context(|| format!("Failed to query cursorDiskKV for key: {}", key))
}

fn parse_global_registry(
    data: &str,
    identity: &WorkspaceIdentity,
    include_archived: bool,
) -> Result<Vec<SessionMetadata>> {
    let json: Value =
        serde_json::from_str(data).context("Failed to parse global composer headers")?;
    let Some(composers) = json.get("allComposers").and_then(|value| value.as_array()) else {
        return Ok(vec![]);
    };

    Ok(composers
        .iter()
        .filter(|value| session_matches_workspace(value, identity))
        .filter_map(|value| parse_session_metadata(value, include_archived))
        .collect())
}

fn parse_local_registry(data: &str, include_archived: bool) -> Result<Vec<SessionMetadata>> {
    let json: Value =
        serde_json::from_str(data).context("Failed to parse workspace composer data")?;
    let Some(composers) = json.get("allComposers").and_then(|value| value.as_array()) else {
        return Ok(vec![]);
    };

    Ok(composers
        .iter()
        .filter_map(|value| parse_session_metadata(value, include_archived))
        .collect())
}

fn parse_session_metadata(value: &Value, include_archived: bool) -> Option<SessionMetadata> {
    let is_archived = value
        .get("isArchived")
        .and_then(|flag| flag.as_bool())
        .unwrap_or(false);
    if is_archived && !include_archived {
        return None;
    }

    let composer_id = value.get("composerId").and_then(|v| v.as_str())?;
    let created_at_ms = value.get("createdAt").and_then(|v| v.as_i64());
    let updated_at_ms = value
        .get("lastUpdatedAt")
        .and_then(|v| v.as_i64())
        .or(created_at_ms);
    let title = value
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string());

    Some(SessionMetadata {
        composer_id: composer_id.to_string(),
        title,
        created_at_ms,
        updated_at_ms,
    })
}

fn session_matches_workspace(value: &Value, identity: &WorkspaceIdentity) -> bool {
    let actual_id = value
        .pointer("/workspaceIdentifier/id")
        .and_then(|v| v.as_str());
    if let (Some(expected_id), Some(actual_id)) = (identity.workspace_id.as_deref(), actual_id) {
        if expected_id == actual_id {
            return true;
        }
    }

    let actual_uri = value
        .pointer("/workspaceIdentifier/uri/external")
        .and_then(|v| v.as_str());
    if let (Some(expected_uri), Some(actual_uri)) =
        (identity.folder_uri_normalized.as_deref(), actual_uri)
    {
        if expected_uri == normalize_uri_for_comparison(actual_uri) {
            return true;
        }
    }

    if identity.is_remote {
        if let (
            Some(expected_authority),
            Some(expected_path),
            Some(actual_authority),
            Some(actual_path),
        ) = (
            identity.remote_authority.as_deref(),
            identity.workspace_path_normalized.as_deref(),
            value
                .pointer("/workspaceIdentifier/uri/authority")
                .and_then(|v| v.as_str()),
            value
                .pointer("/workspaceIdentifier/uri/path")
                .and_then(|v| v.as_str()),
        ) {
            if expected_authority == actual_authority
                && expected_path == normalize_workspace_path(actual_path)
            {
                return true;
            }
        }

        return false;
    }

    if let (Some(expected_path), Some(actual_path)) = (
        identity.workspace_path_normalized.as_deref(),
        value
            .pointer("/workspaceIdentifier/uri/path")
            .and_then(|v| v.as_str()),
    ) {
        if expected_path == normalize_workspace_path(actual_path) {
            return true;
        }
    }

    false
}

fn dedupe_sessions(sessions: &mut Vec<SessionMetadata>) {
    let mut deduped = HashMap::<String, SessionMetadata>::new();

    for session in sessions.drain(..) {
        deduped
            .entry(session.composer_id.clone())
            .and_modify(|existing| merge_session_metadata(existing, &session))
            .or_insert(session);
    }

    sessions.extend(deduped.into_values());
}

fn merge_session_metadata(existing: &mut SessionMetadata, incoming: &SessionMetadata) {
    if existing.title.is_none() {
        existing.title = incoming.title.clone();
    }

    existing.created_at_ms = match (existing.created_at_ms, incoming.created_at_ms) {
        (Some(lhs), Some(rhs)) => Some(lhs.min(rhs)),
        (None, Some(rhs)) => Some(rhs),
        (value, None) => value,
    };

    existing.updated_at_ms = match (existing.updated_at_ms, incoming.updated_at_ms) {
        (Some(lhs), Some(rhs)) => Some(lhs.max(rhs)),
        (None, Some(rhs)) => Some(rhs),
        (value, None) => value,
    };
}

fn exclude_child_sessions_from_sources(
    global_conn: Option<&Connection>,
    local_conn: Option<&Connection>,
    sessions: &mut Vec<SessionMetadata>,
) {
    if sessions.len() <= 1 {
        return;
    }

    let mut child_ids = HashSet::new();
    if let Some(conn) = global_conn {
        collect_child_ids_for_sessions(conn, sessions, &mut child_ids);
    }
    if let Some(conn) = local_conn {
        collect_child_ids_for_sessions(conn, sessions, &mut child_ids);
    }

    if child_ids.is_empty() {
        return;
    }

    sessions.retain(|session| !child_ids.contains(&session.composer_id));
}

fn collect_child_ids_for_sessions(
    conn: &Connection,
    sessions: &[SessionMetadata],
    child_ids: &mut HashSet<String>,
) {
    let session_ids: HashSet<String> = sessions
        .iter()
        .map(|session| session.composer_id.clone())
        .collect();

    for session_id in &session_ids {
        let composer_key = format!("composerData:{}", session_id);
        let Some(data) = query_cursor_disk_value(conn, &composer_key).ok().flatten() else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&data) else {
            continue;
        };

        collect_child_ids(json.get("subComposerIds"), &session_ids, child_ids);
        collect_child_ids(json.get("subagentComposerIds"), &session_ids, child_ids);
    }
}

fn collect_child_ids(
    value: Option<&Value>,
    known_sessions: &HashSet<String>,
    child_ids: &mut HashSet<String>,
) {
    let Some(entries) = value.and_then(|v| v.as_array()) else {
        return;
    };

    for entry in entries {
        let Some(child_id) = entry.as_str() else {
            continue;
        };
        if known_sessions.contains(child_id) {
            child_ids.insert(child_id.to_string());
        }
    }
}

fn sort_sessions(sessions: &mut [SessionMetadata]) {
    sessions.sort_by(|a, b| {
        b.updated_at_ms
            .cmp(&a.updated_at_ms)
            .then_with(|| b.created_at_ms.cmp(&a.created_at_ms))
            .then_with(|| a.composer_id.cmp(&b.composer_id))
    });
}

fn extract_workspace_path(uri: &str) -> Option<String> {
    split_uri(uri).map(|(_, _, path)| normalize_workspace_path(&path))
}

fn uri_scheme(uri: &str) -> Option<String> {
    split_uri(uri).map(|(scheme, _, _)| scheme.to_string())
}

fn normalize_workspace_path(path: &str) -> String {
    let trimmed = path
        .trim_end_matches('/')
        .replace("%3A", ":")
        .replace("%3a", ":");
    let decoded = percent_decode_str(&trimmed).decode_utf8_lossy();
    normalize_drive_letter(&decoded)
}

fn normalize_uri_for_comparison(uri: &str) -> String {
    let trimmed = uri.trim_end_matches('/');
    let Some((scheme, authority, path)) = split_uri(trimmed) else {
        return trimmed.replace("%3A", ":").replace("%3a", ":");
    };

    let path = normalize_workspace_path(&path);

    if authority.is_empty() {
        format!("{}://{}", scheme.to_ascii_lowercase(), path)
    } else {
        format!("{}://{}{}", scheme.to_ascii_lowercase(), authority, path)
    }
}

fn extract_uri_authority(uri: &str) -> Option<String> {
    let (_, authority, _) = split_uri(uri)?;
    (!authority.is_empty()).then_some(authority)
}

fn split_uri(uri: &str) -> Option<(&str, String, String)> {
    let (scheme, rest) = uri.split_once("://")?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };

    Some((scheme, authority.to_string(), path.to_string()))
}

fn normalize_drive_letter(path: &str) -> String {
    let mut chars: Vec<char> = path.chars().collect();

    let drive_index = match chars.as_slice() {
        ['/', drive, ':', '/', ..] if drive.is_ascii_alphabetic() => Some(1),
        [drive, ':', '/', ..] if drive.is_ascii_alphabetic() => Some(0),
        _ => None,
    };

    if let Some(index) = drive_index {
        chars[index] = chars[index].to_ascii_lowercase();
        chars.into_iter().collect()
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn
    }

    fn insert_item(conn: &Connection, key: &str, value: &str) {
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )
        .unwrap();
    }

    fn insert_disk_value(conn: &Connection, key: &str, value: &str) {
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )
        .unwrap();
    }

    #[test]
    fn parse_global_registry_matches_local_workspace_by_id() {
        let headers = r#"{
            "allComposers": [
                {
                    "composerId": "session-a",
                    "name": "Main",
                    "createdAt": 1000,
                    "lastUpdatedAt": 2000,
                    "isArchived": false,
                    "workspaceIdentifier": {
                        "id": "workspace-1",
                        "uri": {
                            "external": "file:///tmp/Project"
                        }
                    }
                },
                {
                    "composerId": "session-b",
                    "name": "Other",
                    "createdAt": 1000,
                    "lastUpdatedAt": 2000,
                    "isArchived": false,
                    "workspaceIdentifier": {
                        "id": "workspace-2",
                        "uri": {
                            "external": "file:///tmp/Other"
                        }
                    }
                }
            ]
        }"#;

        let identity = WorkspaceIdentity {
            workspace_id: Some("workspace-1".to_string()),
            folder_uri_normalized: Some(normalize_uri_for_comparison("file:///tmp/Project")),
            workspace_path_normalized: Some(normalize_workspace_path("/tmp/Project")),
            remote_authority: None,
            is_remote: false,
        };

        let sessions = parse_global_registry(headers, &identity, false).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].composer_id, "session-a");
    }

    #[test]
    fn remote_workspace_requires_matching_authority_and_path() {
        let identity = WorkspaceIdentity {
            workspace_id: Some("workspace-remote".to_string()),
            folder_uri_normalized: Some(normalize_uri_for_comparison(
                "vscode-remote://ssh-remote%2Bhost-a/home/user/project",
            )),
            workspace_path_normalized: Some(normalize_workspace_path("/home/user/project")),
            remote_authority: Some("ssh-remote%2Bhost-a".to_string()),
            is_remote: true,
        };

        let matching = serde_json::json!({
            "workspaceIdentifier": {
                "uri": {
                    "authority": "ssh-remote%2Bhost-a",
                    "path": "/home/user/project"
                }
            }
        });
        let wrong_host = serde_json::json!({
            "workspaceIdentifier": {
                "uri": {
                    "authority": "ssh-remote%2Bhost-b",
                    "path": "/home/user/project"
                }
            }
        });

        assert!(session_matches_workspace(&matching, &identity));
        assert!(!session_matches_workspace(&wrong_host, &identity));
    }

    #[test]
    fn dedupes_global_and_local_sessions_and_prefers_richer_metadata() {
        let mut sessions = vec![
            SessionMetadata {
                composer_id: "session-a".to_string(),
                title: None,
                created_at_ms: Some(2000),
                updated_at_ms: Some(3000),
            },
            SessionMetadata {
                composer_id: "session-a".to_string(),
                title: Some("Recovered title".to_string()),
                created_at_ms: Some(1000),
                updated_at_ms: Some(4000),
            },
        ];

        dedupe_sessions(&mut sessions);

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title.as_deref(), Some("Recovered title"));
        assert_eq!(sessions[0].created_at_ms, Some(1000));
        assert_eq!(sessions[0].updated_at_ms, Some(4000));
    }

    #[test]
    fn exclude_child_sessions_works_with_local_cursor_disk_kv() {
        let conn = init_test_db();
        insert_disk_value(
            &conn,
            "composerData:parent",
            r#"{"subagentComposerIds":["child"]}"#,
        );

        let mut sessions = vec![
            SessionMetadata {
                composer_id: "parent".to_string(),
                title: Some("Parent".to_string()),
                created_at_ms: Some(1000),
                updated_at_ms: Some(2000),
            },
            SessionMetadata {
                composer_id: "child".to_string(),
                title: Some("Child".to_string()),
                created_at_ms: Some(1000),
                updated_at_ms: Some(2000),
            },
        ];

        exclude_child_sessions_from_sources(None, Some(&conn), &mut sessions);

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].composer_id, "parent");
    }

    #[test]
    fn local_registry_ignores_migrated_ui_state_without_all_composers() {
        let conn = init_test_db();
        insert_item(
            &conn,
            LOCAL_COMPOSER_DATA_KEY,
            r#"{"selectedComposerIds":["session-a"],"hasMigratedComposerData":true}"#,
        );

        let sessions = load_legacy_local_sessions(&conn, false).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn normalize_uri_preserves_case_for_posix_paths() {
        assert_eq!(
            normalize_uri_for_comparison("file:///tmp/Project"),
            "file:///tmp/Project"
        );
        assert_ne!(
            normalize_uri_for_comparison("file:///tmp/Project"),
            normalize_uri_for_comparison("file:///tmp/project")
        );
    }

    #[test]
    fn normalize_uri_lowercases_only_windows_drive_letter() {
        assert_eq!(
            normalize_uri_for_comparison("file:///C%3A/Users/me/Project"),
            "file:///c:/Users/me/Project"
        );
    }
}
