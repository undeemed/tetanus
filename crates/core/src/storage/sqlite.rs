//! The database-backed key-value store: the same declared tables of JSON, in
//! one SQLite file instead of one text file.
//!
//! **Why a second backend at all.** A file that is rewritten whole is the
//! right shape for a store holding checkpoints and titles, and the wrong shape
//! once a deployment keeps enough of them that rewriting the file on every
//! write is the cost. A database writes one row. Which of the two a deployment
//! wants is a deployment's decision, which is why both sit behind
//! [`KvStore`](super::KvStore) and neither is named by a caller.
//!
//! **Nothing is written until something is stored.** The connection is opened
//! lazily, so a store that is opened and never written leaves no file at all -
//! the rule the file backend states, kept here rather than excused, because a
//! rule that holds for one backend and not the other is not a rule a caller
//! can rely on. A read before the first write answers empty, as it does there.
//!
//! **The database says what it is.** `application_id` and `user_version` are
//! stamped when the file is created and checked when it is opened, so an
//! unrelated database is refused rather than grown a `records` table, and a
//! file written by a future schema is refused rather than misread. The session
//! store next door does the same, for the same reason.
//!
//! **Durability is per write.** `synchronous = FULL` under WAL fsyncs on every
//! commit, and every `put` and `remove` is its own commit. A backend a caller
//! cannot tell apart must not quietly promise less than the one that fsyncs a
//! file per write.
//!
//! Parity: upstream `packages/storage/storage-sqlite`. Its per-unit schema and
//! its unit/table split come across as the `records` table's columns; its
//! write-behind batching does not, because tetanus commits each write.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use super::{valid_name, KvStore, StorageError, Table};

/// The table layout's version. Bumped only when the columns change.
pub const SCHEMA_VERSION: i32 = 1;

/// `PRAGMA application_id` for a tetanus key-value store: ASCII `tetk`.
pub const APPLICATION_ID: i32 = 0x7465_746b;

/// A key-value store kept in one SQLite database.
pub struct SqliteStore {
    path: PathBuf,
    declared: BTreeSet<String>,
    /// Opened on the first write, and on the first read of a file that is
    /// already there. `None` means "no medium yet", which is the empty store.
    connection: Option<Connection>,
}

impl SqliteStore {
    /// Open the store at `path`, declaring the tables this caller will use.
    ///
    /// A file that is not there yet is not an error and is not created. A
    /// declared table the database does not hold reads as empty, and a table
    /// in the database nobody declared is kept: two components may share one
    /// store, and one of them opening it must not delete the other's rows.
    pub fn open<P: AsRef<Path>>(path: P, declared: &[&str]) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        for name in declared {
            valid_name("table", name)?;
        }
        let mut store = Self {
            path,
            declared: declared.iter().map(|name| (*name).to_string()).collect(),
            connection: None,
        };
        if store.path.exists() {
            store.connect()?;
        }
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open the medium, creating and stamping it when it is not there.
    fn connect(&mut self) -> Result<&Connection, StorageError> {
        if self.connection.is_none() {
            let fresh = !self.path.exists();
            let connection =
                Connection::open(&self.path).map_err(|source| self.unreadable(source))?;
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .map_err(|source| self.unreadable(source))?;
            connection
                .pragma_update(None, "synchronous", "FULL")
                .map_err(|source| self.unreadable(source))?;
            if fresh {
                stamp(&connection).map_err(|source| self.unwritable(source))?;
            } else {
                self.check(&connection)?;
            }
            connection
                .execute(
                    "CREATE TABLE IF NOT EXISTS records (
                        tbl   TEXT NOT NULL,
                        key   TEXT NOT NULL,
                        value TEXT NOT NULL,
                        PRIMARY KEY (tbl, key)
                    )",
                    [],
                )
                .map_err(|source| self.unwritable(source))?;
            self.connection = Some(connection);
        }
        Ok(self.connection.as_ref().expect("just opened"))
    }

    /// Refuse a database that is not one of these, or is one this build cannot
    /// read.
    ///
    /// The identity check comes before the version check: an unrelated
    /// database with a coincidental `user_version` would otherwise be reported
    /// as a version problem, which sends the reader after the wrong answer.
    fn check(&self, connection: &Connection) -> Result<(), StorageError> {
        let id: i32 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .map_err(|source| self.unreadable(source))?;
        // Zero is what every SQLite file that never stamped one carries, so an
        // unstamped database is another program's, not an older one of ours.
        if id != APPLICATION_ID {
            return Err(StorageError::Malformed {
                path: self.path.clone(),
                message: format!(
                    "is not a tetanus key-value store (application_id {id:#x}, expected {APPLICATION_ID:#x})"
                ),
            });
        }
        let version: i32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|source| self.unreadable(source))?;
        if version != SCHEMA_VERSION {
            return Err(StorageError::ForeignVersion {
                path: self.path.clone(),
                found: version.max(0) as u32,
            });
        }
        Ok(())
    }

    fn require_declared(&self, table: &str) -> Result<(), StorageError> {
        match self.declared.contains(table) {
            true => Ok(()),
            false => Err(StorageError::UndeclaredTable {
                name: table.to_string(),
                declared: self.declared.iter().cloned().collect(),
            }),
        }
    }

    fn unreadable(&self, source: rusqlite::Error) -> StorageError {
        StorageError::Unreadable {
            path: self.path.clone(),
            source: std::io::Error::other(source),
        }
    }

    fn unwritable(&self, source: rusqlite::Error) -> StorageError {
        StorageError::Unwritable {
            path: self.path.clone(),
            source: std::io::Error::other(source),
        }
    }
}

/// Mark a freshly created database as one of ours, at this schema.
fn stamp(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.pragma_update(None, "application_id", APPLICATION_ID)?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)
}

impl KvStore for SqliteStore {
    fn get(&self, table: &str, key: &str) -> Result<Option<Value>, StorageError> {
        self.require_declared(table)?;
        let Some(connection) = self.connection.as_ref() else {
            return Ok(None);
        };
        let stored: Option<String> = connection
            .query_row(
                "SELECT value FROM records WHERE tbl = ?1 AND key = ?2",
                rusqlite::params![table, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| self.unreadable(source))?;
        stored.map(|text| decode(&self.path, &text)).transpose()
    }

    fn read_table(&self, name: &str) -> Result<Table, StorageError> {
        self.require_declared(name)?;
        let Some(connection) = self.connection.as_ref() else {
            return Ok(Table::new());
        };
        let mut statement = connection
            .prepare("SELECT key, value FROM records WHERE tbl = ?1 ORDER BY key")
            .map_err(|source| self.unreadable(source))?;
        let rows = statement
            .query_map(rusqlite::params![name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|source| self.unreadable(source))?;
        let mut table = Table::new();
        for row in rows {
            let (key, text) = row.map_err(|source| self.unreadable(source))?;
            table.insert(key, decode(&self.path, &text)?);
        }
        Ok(table)
    }

    fn put(&mut self, table: &str, key: &str, value: Value) -> Result<Option<Value>, StorageError> {
        self.require_declared(table)?;
        valid_name("key", key)?;
        let previous = self.get(table, key)?;
        let text = value.to_string();
        // One statement, one commit, fsynced: the write is durable when this
        // returns, which is what the file backend promises by fsyncing its
        // rename.
        let connection = self.connect()?;
        connection
            .execute(
                "INSERT INTO records (tbl, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT (tbl, key) DO UPDATE SET value = excluded.value",
                rusqlite::params![table, key, text],
            )
            .map_err(|source| StorageError::Unwritable {
                path: self.path.clone(),
                source: std::io::Error::other(source),
            })?;
        Ok(previous)
    }

    fn remove(&mut self, table: &str, key: &str) -> Result<Option<Value>, StorageError> {
        self.require_declared(table)?;
        let previous = self.get(table, key)?;
        if previous.is_none() {
            // Nothing to remove, and nothing to materialize either: a remove
            // that found nothing must not be what creates the file.
            return Ok(None);
        }
        let connection = self.connect()?;
        connection
            .execute(
                "DELETE FROM records WHERE tbl = ?1 AND key = ?2",
                rusqlite::params![table, key],
            )
            .map_err(|source| StorageError::Unwritable {
                path: self.path.clone(),
                source: std::io::Error::other(source),
            })?;
        Ok(previous)
    }

    fn declared(&self) -> Vec<String> {
        self.declared.iter().cloned().collect()
    }
}

/// A stored value, or the report that this database holds something that is
/// not one.
///
/// `Malformed` and not `Unreadable`: the file was read perfectly well and what
/// came back is not a value, which is a different problem with a different
/// answer.
fn decode(path: &Path, text: &str) -> Result<Value, StorageError> {
    serde_json::from_str(text).map_err(|error| StorageError::Malformed {
        path: path.to_path_buf(),
        message: format!("a stored value does not parse as JSON: {error}"),
    })
}
