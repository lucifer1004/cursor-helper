//! Shared SQLite value helpers for Cursor-managed databases.

use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OptionalExtension, Row};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Utf8SqlValue {
    Text(String),
    Blob(Vec<u8>),
}

impl Utf8SqlValue {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text(value) => value,
            Self::Blob(bytes) => {
                std::str::from_utf8(bytes).expect("Utf8SqlValue::Blob must contain valid UTF-8")
            }
        }
    }

    pub fn from_row(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<Self>> {
        match row.get_ref(idx)? {
            ValueRef::Null => Ok(None),
            ValueRef::Text(bytes) => Ok(Some(Self::Text(std::str::from_utf8(bytes)?.to_string()))),
            ValueRef::Blob(bytes) => match std::str::from_utf8(bytes) {
                Ok(_) => Ok(Some(Self::Blob(bytes.to_vec()))),
                Err(_) => Ok(None),
            },
            ValueRef::Integer(_) | ValueRef::Real(_) => Ok(None),
        }
    }

    pub fn write_back(
        &self,
        conn: &Connection,
        query: &str,
        rowid: i64,
    ) -> rusqlite::Result<usize> {
        Ok(match self {
            Self::Text(value) => conn.execute(query, params![value, rowid])?,
            Self::Blob(bytes) => conn.execute(query, params![bytes, rowid])?,
        })
    }
}

pub fn query_optional_utf8_value(
    conn: &Connection,
    query: &str,
    key: &str,
) -> rusqlite::Result<Option<String>> {
    let text_result = conn
        .query_row(query, rusqlite::params![key], |row| {
            Utf8SqlValue::from_row(row, 0)
        })
        .optional()?;
    if let Some(value) = text_result {
        return Ok(value.map(|value| value.as_str().to_string()));
    }

    conn.query_row(query, rusqlite::params![key.as_bytes()], |row| {
        Utf8SqlValue::from_row(row, 0)
    })
    .optional()
    .map(|value| value.flatten().map(|value| value.as_str().to_string()))
}

pub fn query_optional_utf8_string_like_value(
    conn: &Connection,
    query: &str,
    key: &str,
    column_name: &str,
) -> rusqlite::Result<Option<String>> {
    let strict_reader = |row: &Row<'_>| {
        let idx = 0;
        let value = row.get_ref(idx)?;

        match value {
            ValueRef::Null => Ok(None),
            ValueRef::Text(bytes) => Ok(Some(std::str::from_utf8(bytes)?.to_string())),
            ValueRef::Blob(bytes) => Ok(Some(std::str::from_utf8(bytes)?.to_string())),
            ValueRef::Integer(_) | ValueRef::Real(_) => Err(rusqlite::Error::InvalidColumnType(
                idx,
                column_name.to_string(),
                value.data_type(),
            )),
        }
    };

    let text_result = conn
        .query_row(query, rusqlite::params![key], strict_reader)
        .optional()?;
    if let Some(value) = text_result {
        return Ok(value);
    }

    conn.query_row(query, rusqlite::params![key.as_bytes()], strict_reader)
        .optional()
        .map(|value| value.flatten())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn reads_utf8_blob_values() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE data (value BLOB)", []).unwrap();
        conn.execute(
            "INSERT INTO data(value) VALUES (?1)",
            [Vec::from("file:///workspace".as_bytes())],
        )
        .unwrap();

        let value = conn
            .query_row("SELECT value FROM data", [], |row| {
                Utf8SqlValue::from_row(row, 0)
            })
            .unwrap()
            .unwrap();

        assert_eq!(value, Utf8SqlValue::Blob(b"file:///workspace".to_vec()));
        assert_eq!(value.as_str(), "file:///workspace");
    }

    #[test]
    fn skips_invalid_utf8_blob_values() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE data (value BLOB)", []).unwrap();
        conn.execute("INSERT INTO data(value) VALUES (X'80')", [])
            .unwrap();

        let value = conn
            .query_row("SELECT value FROM data", [], |row| {
                Utf8SqlValue::from_row(row, 0)
            })
            .unwrap();

        assert!(value.is_none());
    }

    #[test]
    fn strict_reader_accepts_utf8_blob_values() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE data (key TEXT PRIMARY KEY, value BLOB)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO data(key, value) VALUES (?1, ?2)",
            ("composer", Vec::from("file:///workspace".as_bytes())),
        )
        .unwrap();

        let value = query_optional_utf8_string_like_value(
            &conn,
            "SELECT value FROM data WHERE key = ?1",
            "composer",
            "value",
        )
        .unwrap();

        assert_eq!(value.as_deref(), Some("file:///workspace"));
    }

    #[test]
    fn strict_reader_rejects_integer_values() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE data (key TEXT PRIMARY KEY, value INTEGER)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO data(key, value) VALUES (?1, 42)", ["composer"])
            .unwrap();

        let err = query_optional_utf8_string_like_value(
            &conn,
            "SELECT value FROM data WHERE key = ?1",
            "composer",
            "value",
        )
        .unwrap_err();

        assert!(matches!(err, rusqlite::Error::InvalidColumnType(..)));
    }

    #[test]
    fn strict_reader_rejects_invalid_utf8_blob_values() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE data (key TEXT PRIMARY KEY, value BLOB)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO data(key, value) VALUES (?1, X'80')",
            ["composer"],
        )
        .unwrap();

        let err = query_optional_utf8_string_like_value(
            &conn,
            "SELECT value FROM data WHERE key = ?1",
            "composer",
            "value",
        )
        .unwrap_err();

        assert!(matches!(err, rusqlite::Error::Utf8Error(_)));
    }

    #[test]
    fn permissive_reader_matches_blob_keys() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE data (key BLOB PRIMARY KEY, value BLOB)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO data(key, value) VALUES (?1, ?2)",
            (
                Vec::from("composerData:workspace".as_bytes()),
                Vec::from("file:///workspace".as_bytes()),
            ),
        )
        .unwrap();

        let value = query_optional_utf8_value(
            &conn,
            "SELECT value FROM data WHERE key = ?1",
            "composerData:workspace",
        )
        .unwrap();

        assert_eq!(value.as_deref(), Some("file:///workspace"));
    }

    #[test]
    fn strict_reader_matches_blob_keys() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE data (key BLOB PRIMARY KEY, value BLOB)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO data(key, value) VALUES (?1, ?2)",
            (
                Vec::from("composer".as_bytes()),
                Vec::from("file:///workspace".as_bytes()),
            ),
        )
        .unwrap();

        let value = query_optional_utf8_string_like_value(
            &conn,
            "SELECT value FROM data WHERE key = ?1",
            "composer",
            "value",
        )
        .unwrap();

        assert_eq!(value.as_deref(), Some("file:///workspace"));
    }
}
