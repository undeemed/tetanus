//! Durable key-value storage: named tables of JSON, behind one seam with two
//! backends under it.
//!
//! The session log answers "what happened"; this answers "what did we work
//! out". A projection checkpoint, a computed title, a cache - each is
//! reproducible from the log and expensive enough to be worth keeping, and
//! none of it belongs in an append-only journal of facts.
//!
//! **The seam is what makes the medium a deployment's choice.** [`KvStore`] is
//! the whole vocabulary: declare tables at open, read one value or one table,
//! write one value, remove one. [`json::Store`] keeps them in one file that is
//! replaced whole and atomically; [`sqlite::SqliteStore`] keeps them in a
//! database. A caller holding a `dyn KvStore` cannot tell which answered it,
//! which is the same arrangement `tetanus_session` already has for journals.
//!
//! **The rules are the seam's, not each backend's.** Tables are declared at
//! open and an undeclared one is a caller mistake rather than a table that
//! quietly appears; a declared table the medium does not hold reads as empty;
//! a table in the medium that nobody declared is kept, because two components
//! may share one store; nothing is written until something is stored; and a
//! medium written under a format this build does not read is refused rather
//! than guessed at. `crates/core/tests/storage_backends.rs` asserts every one
//! of them against both backends, which is what a seam with two
//! implementations is for.
//!
//! Parity: upstream `packages/storage` for the backend vocabulary and the
//! named registry, `storage-json` and `storage-sqlite` for the two media. Its
//! domain layer over them - typed specs, events, migrations - is a further
//! package and stays phase (2)/(3).

pub mod domain;
pub mod json;
pub mod registry;
pub mod sqlite;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

pub use domain::{Domain, DomainChanged, DomainRouter, DomainSpec, GlobalSpec, TableSpec};
pub use json::{Store, FORMAT_VERSION};
pub use registry::{SharedStore, StorageRegistry};
pub use sqlite::SqliteStore;

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
    /// Two stores mounted under one name. A configuration mistake, and never
    /// a reason to replace the first: half the consumers would get the other
    /// medium with nothing in a log to say why.
    #[error("a store named {name:?} is already mounted")]
    DuplicateStore { name: String },
    /// A name nobody mounted. The message lists what is mounted, because the
    /// commonest cause is one deployment spelling a name two ways.
    #[error("no store named {name:?} is mounted (mounted: {mounted:?})")]
    UnknownStore { name: String, mounted: Vec<String> },
}

/// What every storage backend serves, and all a caller needs to know.
///
/// The reads answer owned values where [`json::Store`]'s own methods answer
/// borrows, and that is forced rather than chosen: a database cannot hand out
/// a reference into a file it has not read yet. A caller that holds a concrete
/// `Store` and wants the borrow still has it.
pub trait KvStore {
    /// Read one value.
    fn get(&self, table: &str, key: &str) -> Result<Option<Value>, StorageError>;

    /// Read one whole table. A declared table the medium does not hold reads
    /// as empty; a table nobody declared is an error, not an empty answer.
    fn read_table(&self, name: &str) -> Result<Table, StorageError>;

    /// Store one value durably. Answers what that key held before.
    fn put(&mut self, table: &str, key: &str, value: Value) -> Result<Option<Value>, StorageError>;

    /// Remove one value durably. Answers what was there.
    fn remove(&mut self, table: &str, key: &str) -> Result<Option<Value>, StorageError>;

    /// The tables this store was opened with, in name order. For diagnostics
    /// and for the error a caller gets when it names one that is not here.
    fn declared(&self) -> Vec<String>;
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
