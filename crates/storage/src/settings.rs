//! App settings in SQLite (rusqlite). Settings only — analytics data lives
//! in DuckDB. Simple typed key/value via serde_json.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

use crate::StorageError;

pub struct SettingsDb {
    conn: Connection,
}

impl SettingsDb {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StorageError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        Ok(Self { conn })
    }

    pub fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<(), StorageError> {
        let json = serde_json::to_string(value)
            .map_err(|e| StorageError::Corrupt(format!("serialize `{key}`: {e}")))?;
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, json],
        )?;
        Ok(())
    }

    /// None when missing; stored values that no longer deserialize (e.g.
    /// after a schema change) are treated as missing rather than fatal.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, StorageError> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_roundtrip_and_overwrite() {
        let db = SettingsDb::open_in_memory().unwrap();
        assert_eq!(db.get::<f32>("zoom").unwrap(), None);
        db.set("zoom", &0.5f32).unwrap();
        assert_eq!(db.get::<f32>("zoom").unwrap(), Some(0.5));
        db.set("zoom", &0.75f32).unwrap();
        assert_eq!(db.get::<f32>("zoom").unwrap(), Some(0.75));
        // Type change is treated as missing, not an error.
        db.set("zoom", &"not a float").unwrap();
        assert_eq!(db.get::<f32>("zoom").unwrap(), None);
    }
}
