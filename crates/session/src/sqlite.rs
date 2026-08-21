//! The second session-persistence backend: one SQLite database holding many
//! journals, behind the same [`SessionLog`] seam the JSONL journal serves.
//!
//! **The seam is the point.** A caller holds a `dyn SessionLog` and cannot
//! tell which backend answered it, which is what makes the choice a
//! deployment's rather than the engine's. Everything a JSONL journal promises
//! is promised here: `seq` is the log length at append time, an append is
//! durable when it returns, and [`replay`](SqliteSessionStore::replay) hands
//! back exactly what was written.
//!
//! **One database, many sessions.** A JSONL journal is one file per session
//! because a file is the only structure a text log has. A database has tables,
//! so a deployment that would rather keep one artifact than a directory of
//! thousands keeps one, and the session id is a column instead of a file name.
//!
//! **Durability is per append, not per checkpoint.** `synchronous = FULL`
//! under WAL fsyncs the write-ahead log on every commit, and every append is
//! its own commit. That is deliberately the slower pragma: the JSONL backend
//! fsyncs each record, and a backend a caller cannot tell apart must not
//! quietly promise less.
//!
//! **The database says what it is.** `application_id` and `user_version` are
//! stamped when the file is created and checked when it is opened, so an
//! unrelated database is refused rather than grown a `sessions` table, and a
//! file written by a future schema is refused rather than misread.
//!
//! Migration between the two backends is [`import_jsonl`] and
//! [`export_jsonl`]. The round trip is lossless in both directions: a journal
//! imported and exported again is the same bytes, because both writers
//! serialize the same [`SessionEvent`].
//!
//! Parity: upstream `packages/session/session-persistence-sqlite`. Its
//! packed-chunk rows, revision tokens, write-behind coordinator and
//! incarnation identity serve a batching persistence layer tetanus does not
//! have - tetanus commits each append - so what ports is the schema's shape
//! and the ownership check on open.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension};

use tetanus_core::EventBus;

use crate::{now_ms, SessionError, SessionEvent, SessionEventDispatch, SessionLog};

/// The on-disk table layout's version. Bumped only when the columns change.
pub const SCHEMA_VERSION: i32 = 1;

/// `PRAGMA application_id` for a tetanus session database: ASCII `tets`.
///
/// A database with a different one is somebody else's, and is refused before
/// a single table is created in it.
pub const APPLICATION_ID: i32 = 0x7465_7473;

fn store_error(error: rusqlite::Error) -> SessionError {
    SessionError::Store(error.to_string())
}

/// One SQLite database holding any number of session journals.
#[derive(Debug)]
pub struct SqliteSessionStore {
    path: PathBuf,
    connection: Mutex<Connection>,
}

impl SqliteSessionStore {
    /// Open - and create when absent - the database at `path`.
    ///
    /// An empty file is stamped with this build's identity and schema. A file
    /// that already carries someone else's identity, or a schema this build
    /// does not read, is refused rather than migrated in place: guessing at
    /// the columns of an unknown layout is how a store loses a log.
    pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>, SessionError> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let connection = Connection::open(&path).map_err(store_error)?;
        configure(&connection, &path)?;
        Ok(Arc::new(Self {
            path,
            connection: Mutex::new(connection),
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open one session's journal, creating its row when the id is new.
    ///
    /// The row exists from this call, as a JSONL journal's file does: a
    /// session that is open but has appended nothing is a session that
    /// [`session_ids`](Self::session_ids) reports, on both backends.
    pub fn log(
        &self,
        id: impl Into<String>,
        bus: EventBus,
    ) -> Result<Arc<SqliteSessionLog>, SessionError> {
        let id = id.into();
        {
            let connection = self.connection.lock().expect("session store");
            connection
                .execute(
                    "INSERT OR IGNORE INTO sessions (id, created_time) VALUES (?1, ?2)",
                    rusqlite::params![&id, now_ms() as i64],
                )
                .map_err(store_error)?;
        }
        let events = self.read(&id)?;
        Ok(Arc::new(SqliteSessionLog {
            id,
            bus,
            state: Mutex::new(events),
            store: self.handle()?,
        }))
    }

    /// Every session this database holds, in id order.
    pub fn session_ids(&self) -> Result<Vec<String>, SessionError> {
        let connection = self.connection.lock().expect("session store");
        let mut statement = connection
            .prepare("SELECT id FROM sessions ORDER BY id")
            .map_err(store_error)?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(store_error)?;
        Ok(ids)
    }

    /// Whether this database holds a journal under `id`.
    pub fn contains(&self, id: &str) -> Result<bool, SessionError> {
        let connection = self.connection.lock().expect("session store");
        let found: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_error)?;
        Ok(found.is_some())
    }

    /// Read one journal back, checking `seq` contiguity as [`crate::replay`]
    /// does. A gap means the rows are not a faithful copy of the log that
    /// wrote them.
    pub fn replay(&self, id: &str) -> Result<Vec<SessionEvent>, SessionError> {
        let events = self.read(id)?;
        for (i, event) in events.iter().enumerate() {
            if event.seq != i as u64 {
                return Err(SessionError::Corrupt(i + 1));
            }
        }
        Ok(events)
    }

    /// Write a whole journal at once, from events that already carry their
    /// `seq` and `time` - the SQLite peer of [`crate::seed`], and what a fork
    /// or an import lays down.
    ///
    /// The id must be free, and the rules are the seed's: a seed written over
    /// a journal that already holds a history would splice two histories, and
    /// every seq after the join would name the wrong row.
    pub fn seed(&self, id: &str, events: &[SessionEvent]) -> Result<(), SessionError> {
        for (i, event) in events.iter().enumerate() {
            if event.seq != i as u64 {
                return Err(SessionError::Corrupt(i + 1));
            }
        }
        if self.contains(id)? {
            return Err(SessionError::Exists(id.to_string()));
        }
        let mut connection = self.connection.lock().expect("session store");
        let transaction = connection.transaction().map_err(store_error)?;
        transaction
            .execute(
                "INSERT INTO sessions (id, created_time) VALUES (?1, ?2)",
                rusqlite::params![id, events.first().map_or_else(now_ms, |e| e.time) as i64],
            )
            .map_err(store_error)?;
        for event in events {
            insert(&transaction, id, event)?;
        }
        // One barrier for the whole seed: no caller has been told any part of
        // this is durable until all of it is.
        transaction.commit().map_err(store_error)?;
        Ok(())
    }

    /// Every event of one journal, in seq order, without the contiguity check.
    fn read(&self, id: &str) -> Result<Vec<SessionEvent>, SessionError> {
        let connection = self.connection.lock().expect("session store");
        let mut statement = connection
            .prepare(
                "SELECT type, seq, time, data, source_event_seqs FROM events \
                 WHERE session_id = ?1 ORDER BY seq",
            )
            .map_err(store_error)?;
        let rows = statement
            .query_map(rusqlite::params![id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(store_error)?;
        let mut events = Vec::new();
        for (index, row) in rows.enumerate() {
            let (ty, seq, time, data, sources) = row.map_err(store_error)?;
            // A row whose JSON does not parse is refused naming its position,
            // exactly as a damaged JSONL line is: the log it came from is not
            // the log in the database.
            let data = serde_json::from_str(&data).map_err(|_| SessionError::Corrupt(index + 1))?;
            let source_event_seqs = match sources {
                Some(text) => Some(
                    serde_json::from_str(&text).map_err(|_| SessionError::Corrupt(index + 1))?,
                ),
                None => None,
            };
            events.push(SessionEvent {
                ty,
                seq: seq as u64,
                time: time as u64,
                data,
                source_event_seqs,
            });
        }
        Ok(events)
    }

    /// A second connection to the same file, for a log handle to own.
    ///
    /// A handle keeps its own connection rather than borrowing the store's, so
    /// an append never waits on a listing; SQLite serializes the writers
    /// itself.
    fn handle(&self) -> Result<Mutex<Connection>, SessionError> {
        let connection = Connection::open(&self.path).map_err(store_error)?;
        pragmas(&connection)?;
        Ok(Mutex::new(connection))
    }
}

/// One session's journal inside a [`SqliteSessionStore`].
///
/// The events are mirrored in memory exactly as the JSONL journal mirrors
/// them, so deriving history never costs a query.
pub struct SqliteSessionLog {
    id: String,
    bus: EventBus,
    state: Mutex<Vec<SessionEvent>>,
    store: Mutex<Connection>,
}

impl SqliteSessionLog {
    fn write(
        &self,
        ty: &str,
        data: serde_json::Value,
        sources: Option<Vec<u64>>,
    ) -> Result<SessionEvent, SessionError> {
        let event = {
            let mut events = self.state.lock().expect("session lock");
            let event = SessionEvent {
                ty: ty.to_string(),
                seq: events.len() as u64,
                time: now_ms(),
                data,
                source_event_seqs: sources,
            };
            let connection = self.store.lock().expect("session store");
            insert(&connection, &self.id, &event)?;
            drop(connection);
            events.push(event.clone());
            event
        };
        self.bus.emit(&SessionEventDispatch {
            event: event.clone(),
        });
        Ok(event)
    }
}

impl SessionLog for SqliteSessionLog {
    fn id(&self) -> &str {
        &self.id
    }

    fn append(&self, ty: &str, data: serde_json::Value) -> Result<SessionEvent, SessionError> {
        self.write(ty, data, None)
    }

    fn append_with_sources(
        &self,
        ty: &str,
        data: serde_json::Value,
        sources: Vec<u64>,
    ) -> Result<SessionEvent, SessionError> {
        self.write(ty, data, Some(sources))
    }

    fn events(&self) -> Vec<SessionEvent> {
        self.state.lock().expect("session lock").clone()
    }

    fn flush(&self) -> Result<(), SessionError> {
        // Every append is its own committed transaction under
        // `synchronous = FULL`, so the barrier has already been crossed by the
        // time a caller asks for it. It is still served, because a caller that
        // awaits durability must not have to know which backend it is on.
        Ok(())
    }
}

/// Copy a JSONL journal into the database under `id`.
///
/// The events keep the seqs and the times they were written under, so an
/// imported journal derives exactly the history the file did.
pub fn import_jsonl(
    store: &SqliteSessionStore,
    id: &str,
    path: impl AsRef<Path>,
) -> Result<usize, SessionError> {
    let events = crate::replay(path)?;
    store.seed(id, &events)?;
    Ok(events.len())
}

/// Copy one journal out of the database into a JSONL file.
///
/// The file must not exist, for [`crate::seed`]'s reason: writing a journal
/// onto one that already holds a history would splice the two.
pub fn export_jsonl(
    store: &SqliteSessionStore,
    id: &str,
    path: impl AsRef<Path>,
) -> Result<usize, SessionError> {
    let events = store.replay(id)?;
    crate::seed(path, &events)?;
    Ok(events.len())
}

fn insert(
    connection: &Connection,
    session_id: &str,
    event: &SessionEvent,
) -> Result<(), SessionError> {
    let data = serde_json::to_string(&event.data)
        .map_err(|_| SessionError::NotSerializable(event.ty.clone()))?;
    let sources = match &event.source_event_seqs {
        Some(seqs) => Some(
            serde_json::to_string(seqs)
                .map_err(|_| SessionError::NotSerializable(event.ty.clone()))?,
        ),
        None => None,
    };
    connection
        .execute(
            "INSERT INTO events (session_id, seq, type, time, data, source_event_seqs) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                session_id,
                event.seq as i64,
                &event.ty,
                event.time as i64,
                data,
                sources
            ],
        )
        .map_err(store_error)?;
    Ok(())
}

/// Stamp a new database, or check the identity of one that already exists.
fn configure(connection: &Connection, path: &Path) -> Result<(), SessionError> {
    pragmas(connection)?;
    let application: i32 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(store_error)?;
    let version: i32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(store_error)?;
    let objects: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .map_err(store_error)?;

    // A file with no identity and no tables is ours to claim. One with tables
    // and no identity belongs to something else, and creating a `sessions`
    // table in it would be a write into a stranger's database.
    if application == 0 && version == 0 && objects == 0 {
        connection
            .execute_batch(&format!(
                "PRAGMA application_id = {APPLICATION_ID};
                 PRAGMA user_version = {SCHEMA_VERSION};"
            ))
            .map_err(store_error)?;
    } else if application != APPLICATION_ID {
        return Err(SessionError::ForeignStore {
            path: path.to_path_buf(),
            found: application,
        });
    } else if version != SCHEMA_VERSION {
        return Err(SessionError::ForeignSchema {
            path: path.to_path_buf(),
            found: version,
        });
    }

    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                 id           TEXT PRIMARY KEY,
                 created_time INTEGER NOT NULL
             ) STRICT;
             CREATE TABLE IF NOT EXISTS events (
                 session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 seq               INTEGER NOT NULL,
                 type              TEXT NOT NULL,
                 time              INTEGER NOT NULL,
                 data              TEXT NOT NULL,
                 source_event_seqs TEXT,
                 PRIMARY KEY (session_id, seq)
             ) STRICT;",
        )
        .map_err(store_error)?;
    Ok(())
}

/// The pragmas every connection to a session database runs under.
fn pragmas(connection: &Connection) -> Result<(), SessionError> {
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(store_error)?;
    Ok(())
}
