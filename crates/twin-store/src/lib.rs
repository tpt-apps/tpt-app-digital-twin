//! Local Keystone database layer for twin state, results, sensor history and version metadata.
//!
//! Twins are stored as validated JSON documents in a single Keystone table.
//! Everything stays on disk under one directory, with no network access.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tpt_keystone::executor::execute_query;
use tpt_keystone::storage::config::{NodeRole, UdfConfig};
use tpt_keystone::storage::database::Database;
use tpt_keystone::storage::lease::LeaseManager;
use tpt_keystone::storage::objectstore::{LocalFsObjectStore, ObjectStore};
use twin_core::{Twin, TwinError, TwinId};

const TABLE: &str = "twins";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Database(String),
    #[error(transparent)]
    Twin(#[from] TwinError),
}

fn db_err(e: anyhow::Error) -> StoreError {
    StoreError::Database(format!("{e:#}"))
}

/// A local store of twin documents.
pub struct TwinStore {
    db: Arc<Database>,
}

impl TwinStore {
    /// Open or create a store in `dir`. Keystone keeps its write-ahead log and
    /// object store under this directory.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        let bucket = dir.join("objects");
        std::fs::create_dir_all(&bucket)?;
        let store: Arc<dyn ObjectStore> =
            Arc::new(LocalFsObjectStore::open(&bucket).map_err(db_err)?);
        let lease = Arc::new(LeaseManager::new(
            store.clone(),
            "twin",
            "local".into(),
            Duration::from_secs(30),
        ));
        lease.try_acquire().map_err(db_err)?;
        let db = Database::open(
            &dir.join("local"),
            store,
            lease.handle(),
            NodeRole::Writer,
            UdfConfig::default(),
        )
        .map_err(db_err)?;
        let db = Arc::new(db);
        execute_query(
            &format!(
                "CREATE TABLE IF NOT EXISTS {TABLE} (id TEXT PRIMARY KEY, name TEXT, doc TEXT)"
            ),
            db.clone(),
        )
        .map_err(db_err)?;
        Ok(Self { db })
    }

    /// Insert or replace a twin. The twin is validated before it is written.
    pub fn save(&self, twin: &Twin) -> Result<(), StoreError> {
        let json = twin.to_json()?;
        let id = sql_text(&twin.id.to_string());
        let name = sql_text(&twin.name);
        let doc = sql_text(&json);
        self.run(&format!("DELETE FROM {TABLE} WHERE id = {id}"))?;
        self.run(&format!(
            "INSERT INTO {TABLE} (id, name, doc) VALUES ({id}, {name}, {doc})"
        ))?;
        Ok(())
    }

    /// Load a twin by ID, or `None` if it is not stored.
    pub fn load(&self, id: TwinId) -> Result<Option<Twin>, StoreError> {
        let rows = self.query(&format!(
            "SELECT doc FROM {TABLE} WHERE id = {}",
            sql_text(&id.to_string())
        ))?;
        match rows.first() {
            None => Ok(None),
            Some(json) => Ok(Some(Twin::from_json(json)?)),
        }
    }

    /// List the stored twins as `(id, name)` pairs, ordered by name.
    pub fn list(&self) -> Result<Vec<(TwinId, String)>, StoreError> {
        let rows = self.query_rows(&format!("SELECT id, name FROM {TABLE} ORDER BY name"))?;
        rows.into_iter()
            .map(|(id, name)| Ok((TwinId::parse(&id)?, name)))
            .collect()
    }

    pub fn delete(&self, id: TwinId) -> Result<(), StoreError> {
        self.run(&format!(
            "DELETE FROM {TABLE} WHERE id = {}",
            sql_text(&id.to_string())
        ))
    }

    fn run(&self, sql: &str) -> Result<(), StoreError> {
        execute_query(sql, self.db.clone()).map_err(db_err)?;
        Ok(())
    }

    /// Run a query and return the first column of each row as text.
    fn query(&self, sql: &str) -> Result<Vec<String>, StoreError> {
        Ok(self.query_rows(sql)?.into_iter().map(|row| row.0).collect())
    }

    /// Run a query and return the first two columns of each row as text.
    fn query_rows(&self, sql: &str) -> Result<Vec<(String, String)>, StoreError> {
        let result = execute_query(sql, self.db.clone()).map_err(db_err)?;
        let text = |cell: &Option<Vec<u8>>| {
            cell.as_deref()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default()
        };
        Ok(result
            .rows
            .iter()
            .map(|row| {
                (
                    row.first().map(text).unwrap_or_default(),
                    row.get(1).map(text).unwrap_or_default(),
                )
            })
            .collect())
    }
}

/// Quote a value as a SQL string literal, doubling embedded single quotes.
fn sql_text(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("twin-store-test-{tag}-{}", TwinId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = temp_dir("roundtrip");
        let store = TwinStore::open(&dir).unwrap();
        let mut twin = Twin::new("Robot arm").unwrap();
        twin.set_parameter("mass_kg", 2.5).unwrap();
        store.save(&twin).unwrap();

        assert_eq!(store.load(twin.id).unwrap(), Some(twin));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn save_replaces_existing_twin() {
        let dir = temp_dir("replace");
        let store = TwinStore::open(&dir).unwrap();
        let mut twin = Twin::new("v1").unwrap();
        store.save(&twin).unwrap();
        twin.name = "v2".into();
        store.save(&twin).unwrap();

        let listed = store.list().unwrap();
        assert_eq!(listed, vec![(twin.id, "v2".to_string())]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_twin_loads_as_none() {
        let dir = temp_dir("missing");
        let store = TwinStore::open(&dir).unwrap();
        assert_eq!(store.load(TwinId::new()).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn delete_removes_twin() {
        let dir = temp_dir("delete");
        let store = TwinStore::open(&dir).unwrap();
        let twin = Twin::new("gone").unwrap();
        store.save(&twin).unwrap();
        store.delete(twin.id).unwrap();
        assert_eq!(store.load(twin.id).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn quotes_in_names_are_stored_safely() {
        let dir = temp_dir("quotes");
        let store = TwinStore::open(&dir).unwrap();
        let twin = Twin::new("Bob's \"arm\"; DROP TABLE twins").unwrap();
        store.save(&twin).unwrap();
        assert_eq!(store.load(twin.id).unwrap(), Some(twin));
        let _ = std::fs::remove_dir_all(dir);
    }
}
