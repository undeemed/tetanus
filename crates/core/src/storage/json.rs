//! The file-backed key-value store: named tables of JSON, in one file that is
//! replaced whole and atomically.
//!
//! **The file is replaced whole, atomically.** A write goes to a temporary
//! file in the same directory, is fsynced, and is renamed over the target;
//! then the directory is fsynced so the new entry survives a crash. There is
//! no in-place edit and no partial write to be found later, which is what lets
//! a reader trust a file it did not write.
//!
//! **Memory never gets ahead of the disk.** A publish that fails rolls the
//! in-memory tables back to what is actually stored, so a caller that ignores
//! the error still reads what a fresh open would read. The alternative -
//! remembering a value that was never written - is the failure mode that only
//! shows up after a restart, which is the worst time to find it.
//!
//! **Nothing is written until something is stored.** Opening a store that has
//! never been written creates no file, so a run that stores nothing leaves no
//! trace.
//!
//! The rules this shares with the other backend are stated once, on
//! [`super::KvStore`], and asserted against both in
//! `crates/core/tests/storage_backends.rs`.
//!
//! Parity: upstream `packages/storage/storage-json`, pinned by its
//! `json-backend.spec.ts`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{valid_name, KvStore, StorageError, Table};

/// The format marker every store file carries.
///
/// A file whose version is not this one is refused rather than guessed at: the
/// tables are the point, and reading them under the wrong rules would hand a
/// caller values that mean something else.
pub const FORMAT_VERSION: u32 = 1;

/// Named tables of JSON, persisted in one file.
#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    tables: BTreeMap<String, Table>,
}

impl Store {
    /// Open the store at `path`, declaring the tables this caller will use.
    ///
    /// A file that is not there yet is not an error and is not created: it is
    /// an empty store, and the first write materializes it. A declared table
    /// the file does not hold reads as empty, so adding a table to a
    /// deployment does not require migrating its file.
    ///
    /// A table in the file that nobody declared is *kept*, not dropped. Two
    /// components may share one store, and rewriting the file must not delete
    /// the tables belonging to whichever of them did not open it this time.
    pub fn open<P: AsRef<Path>>(path: P, declared: &[&str]) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        for name in declared {
            valid_name("table", name)?;
        }

        let mut tables = match std::fs::read_to_string(&path) {
            Ok(text) => parse(&path, &text)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            // Anything other than "not there" is a real failure and is not a
            // reason to start from empty: doing that would silently discard a
            // store the filesystem merely refused to hand over this time.
            Err(source) => return Err(StorageError::Unreadable { path, source }),
        };
        for name in declared {
            tables.entry((*name).to_string()).or_default();
        }
        Ok(Self { path, tables })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read one value.
    pub fn get(&self, table: &str, key: &str) -> Result<Option<&Value>, StorageError> {
        Ok(self.table(table)?.get(key))
    }

    /// Read one whole table.
    pub fn table(&self, name: &str) -> Result<&Table, StorageError> {
        self.tables
            .get(name)
            .ok_or_else(|| StorageError::UndeclaredTable {
                name: name.to_string(),
                declared: self.tables.keys().cloned().collect(),
            })
    }

    /// Store one value, and publish the whole store.
    ///
    /// Publishing on every write rather than on a later flush is deliberate at
    /// this size: a store holds checkpoints and titles, not a hot loop's
    /// working set, and a caller that has to remember to flush is a caller
    /// that eventually does not.
    pub fn put(
        &mut self,
        table: &str,
        key: &str,
        value: Value,
    ) -> Result<Option<Value>, StorageError> {
        valid_name("key", key)?;
        self.mutate(table, |t| t.insert(key.to_string(), value))
    }

    /// Remove one value, and publish. Answers what was there.
    ///
    /// A remove that found nothing publishes nothing. Found by the conformance
    /// suite the two backends share (TC-PORT-STORE-C5): clearing a key that
    /// was never set used to write the file, so a run whose only storage call
    /// was a defensive `remove` left a store behind - which is exactly the
    /// "nothing is written until something is stored" rule this module opens
    /// with, broken by the one operation that looks like it changes nothing.
    pub fn remove(&mut self, table: &str, key: &str) -> Result<Option<Value>, StorageError> {
        if self.table(table)?.get(key).is_none() {
            return Ok(None);
        }
        self.mutate(table, |t| t.remove(key))
    }

    /// Apply one change and publish it, rolling back if the publish fails.
    fn mutate<T>(
        &mut self,
        table: &str,
        change: impl FnOnce(&mut Table) -> T,
    ) -> Result<T, StorageError> {
        if !self.tables.contains_key(table) {
            return Err(StorageError::UndeclaredTable {
                name: table.to_string(),
                declared: self.tables.keys().cloned().collect(),
            });
        }
        // The whole map, because a publish writes the whole file: rolling back
        // one table would leave the others describing a file that was never
        // written.
        let restore = self.tables.clone();
        let answer = change(self.tables.get_mut(table).expect("checked"));
        match self.publish() {
            Ok(()) => Ok(answer),
            Err(error) => {
                self.tables = restore;
                Err(error)
            }
        }
    }

    /// Write the whole store, atomically.
    fn publish(&self) -> Result<(), StorageError> {
        let document = serde_json::json!({
            "version": FORMAT_VERSION,
            "tables": &self.tables,
        });
        // Pretty, because a store is small and a human reading one during an
        // incident should not have to pipe it through a formatter first.
        let text =
            serde_json::to_string_pretty(&document).map_err(|source| StorageError::Unwritable {
                path: self.path.clone(),
                source: std::io::Error::other(source),
            })?;

        let parent = self.path.parent().unwrap_or(Path::new("."));
        let temporary = parent.join(format!(
            ".{}.tmp",
            self.path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "store".into())
        ));

        let written = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(text.as_bytes())?;
            // The temp file's own contents must be on disk before the rename
            // makes it the store, or a crash can publish an empty file.
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)?;
            // Rename is atomic, but the directory entry itself needs syncing
            // or a crash can lose the rename and leave the old file.
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
            Ok(())
        })();

        if let Err(source) = written {
            // Never leave the temporary behind: it would accumulate, and a
            // reader that found one might take it for the store.
            let _ = std::fs::remove_file(&temporary);
            return Err(StorageError::Unwritable {
                path: self.path.clone(),
                source,
            });
        }
        Ok(())
    }
}

/// Read a store file into its tables.
fn parse(path: &Path, text: &str) -> Result<BTreeMap<String, Table>, StorageError> {
    let document: Value = serde_json::from_str(text).map_err(|error| StorageError::Malformed {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;

    let Some(object) = document.as_object() else {
        return Err(StorageError::Malformed {
            path: path.to_path_buf(),
            message: "the root must be an object".into(),
        });
    };

    // The version is read before the tables: a foreign file's tables may look
    // fine and mean something else, so refusing on shape first would report
    // the wrong problem.
    match object.get("version").and_then(Value::as_u64) {
        Some(found) if found == u64::from(FORMAT_VERSION) => {}
        Some(found) => {
            return Err(StorageError::ForeignVersion {
                path: path.to_path_buf(),
                found: found as u32,
            })
        }
        None => {
            return Err(StorageError::Malformed {
                path: path.to_path_buf(),
                message: "no format version".into(),
            })
        }
    }

    let tables = object
        .get("tables")
        .and_then(Value::as_object)
        .ok_or_else(|| StorageError::Malformed {
            path: path.to_path_buf(),
            message: "`tables` must be an object".into(),
        })?;

    tables
        .iter()
        .map(|(name, contents)| {
            let table = contents
                .as_object()
                .ok_or_else(|| StorageError::Malformed {
                    path: path.to_path_buf(),
                    message: format!("table {name:?} must be an object"),
                })?
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            Ok((name.clone(), table))
        })
        .collect()
}

impl KvStore for Store {
    fn get(&self, table: &str, key: &str) -> Result<Option<Value>, StorageError> {
        Ok(Store::get(self, table, key)?.cloned())
    }

    fn read_table(&self, name: &str) -> Result<Table, StorageError> {
        Ok(Store::table(self, name)?.clone())
    }

    fn put(&mut self, table: &str, key: &str, value: Value) -> Result<Option<Value>, StorageError> {
        Store::put(self, table, key, value)
    }

    fn remove(&mut self, table: &str, key: &str) -> Result<Option<Value>, StorageError> {
        Store::remove(self, table, key)
    }

    fn declared(&self) -> Vec<String> {
        self.tables.keys().cloned().collect()
    }
}
