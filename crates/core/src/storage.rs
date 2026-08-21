//! A small durable key-value store: named tables of JSON, in one file that is
//! replaced whole and atomically.
//!
//! The session log answers "what happened"; this answers "what did we work
//! out". A projection checkpoint, a computed title, a cache - each is
//! reproducible from the log and expensive enough to be worth keeping, and
//! none of it belongs in an append-only journal of facts.
//!
//! **Tables are declared when the store is opened.** Reaching for one that was
//! not declared is a caller mistake and is reported as one, rather than
//! creating it. A typo would otherwise write to a table nobody reads, which is
//! indistinguishable from the data being lost.
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
//! Parity: upstream `packages/storage/storage-json`, pinned by its
//! `json-backend.spec.ts`. Its SQLite backend and the domain layer over both
//! are separate packages and stay phase (2)/(3); the registry that lets a
//! deployment choose between them is only worth having once there are two.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The format marker every store file carries.
///
/// A file whose version is not this one is refused rather than guessed at: the
/// tables are the point, and reading them under the wrong rules would hand a
/// caller values that mean something else.
pub const FORMAT_VERSION: u32 = 1;

/// One table's contents.
pub type Table = BTreeMap<String, Value>;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{}: cannot be read: {source}", path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: cannot be written: {source}", path.display())]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file is not a store this build can read at all.
    #[error("{}: does not parse as a store: {message}", path.display())]
    Malformed { path: PathBuf, message: String },
    /// The file is a store, written by something else. Distinct from
    /// [`Malformed`](Self::Malformed) because the answers differ: one is a
    /// corrupt file, the other is a file from a version that may still be
    /// running.
    #[error("{}: is format version {found}, and this build reads {FORMAT_VERSION}", path.display())]
    ForeignVersion { path: PathBuf, found: u32 },
    /// A table that was not declared at open. A caller error, and never a
    /// reason to create one.
    #[error("no table {name:?} was declared for this store (declared: {declared:?})")]
    UndeclaredTable { name: String, declared: Vec<String> },
    #[error("{what} name {name:?} must be 1 to 64 characters of [a-z0-9._-]")]
    BadName { what: &'static str, name: String },
}

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
    pub fn remove(&mut self, table: &str, key: &str) -> Result<Option<Value>, StorageError> {
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

/// Whether a name is one this store will accept.
///
/// The character set is the one that is safe in a file name, a JSON key and a
/// log line at once, so a name never has to be escaped differently depending
/// on where it is being shown.
fn valid_name(what: &'static str, name: &str) -> Result<(), StorageError> {
    let shaped = (1..=64).contains(&name.len())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    match shaped {
        true => Ok(()),
        false => Err(StorageError::BadName {
            what,
            name: name.to_string(),
        }),
    }
}
