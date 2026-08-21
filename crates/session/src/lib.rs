//! Session log: the append-only journal of durable `SessionEvent`s that is the
//! source of the context a model sees. Model history is *derived* from this log,
//! never stored beside it, so replay is re-derivation from the same events.
//!
//! Shape parity with upstream (`docs/subsystems/session.md`, the
//! `SessionEvent<T>` log entry): a discriminated union over `type`, a `seq`
//! equal to the log length at append time, `time` in epoch milliseconds, a JSON
//! `data` payload, and - on the three surface event types - the
//! `sourceEventSeqs` an event cites.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tetanus_core::events::{DispatchMode, Event, EventBus};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("corrupt journal line {0}")]
    Corrupt(usize),
    #[error("event data for {0:?} is not JSON-serializable")]
    NotSerializable(String),
}

/// One immutable entry in the session log.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionEvent {
    /// Event type, e.g. `turn/start`, `assistant/chunk`.
    #[serde(rename = "type")]
    pub ty: String,
    /// Monotonic position in the log: `seq == log.len()` at append time.
    pub seq: u64,
    /// Unix epoch milliseconds.
    pub time: u64,
    pub data: serde_json::Value,
    /// Seq numbers of the earlier events this one cites as sources. Present
    /// only on surface events (`user/message`, `assistant/message`,
    /// `tool/result`); an `assistant/message` may cite a known-empty list.
    #[serde(rename = "sourceEventSeqs", skip_serializing_if = "Option::is_none")]
    pub source_event_seqs: Option<Vec<u64>>,
}

/// Broadcast of one durable fact. Every append publishes it, so observers -
/// transcripts, the CLI printer, telemetry - never poll the file.
pub struct SessionEventDispatch {
    pub event: SessionEvent,
}
impl Event for SessionEventDispatch {
    const TOPIC: &'static str = "session/event";
    const MODE: DispatchMode = DispatchMode::Emit;
    type Output = ();
}

/// The explicit durability barrier a producer awaits when it needs the log on
/// disk before it continues (upstream `ctx.sessions.flush(session)`).
pub struct SessionFlush {
    pub session_id: String,
}
impl Event for SessionFlush {
    const TOPIC: &'static str = "session/flush";
    const MODE: DispatchMode = DispatchMode::Parallel;
    type Output = ();
}

/// The session capability seam. Phase ① ships one provider, the JSONL journal.
pub trait SessionLog: Send + Sync {
    fn id(&self) -> &str;
    /// Append a durable fact, assigning its `seq` and `time`.
    fn append(&self, ty: &str, data: serde_json::Value) -> Result<SessionEvent, SessionError>;
    /// Append a surface event citing the earlier events that produced it.
    fn append_with_sources(
        &self,
        ty: &str,
        data: serde_json::Value,
        sources: Vec<u64>,
    ) -> Result<SessionEvent, SessionError>;
    /// Every event appended so far, in order.
    fn events(&self) -> Vec<SessionEvent>;
    /// Force the journal to stable storage.
    fn flush(&self) -> Result<(), SessionError>;
}

struct State {
    file: std::fs::File,
    events: Vec<SessionEvent>,
}

/// Append-only JSONL journal: one `SessionEvent` per line, fsynced on append,
/// mirrored in memory so history derivation never re-reads the file.
pub struct JsonlSessionLog {
    id: String,
    path: PathBuf,
    bus: EventBus,
    state: Mutex<State>,
}

impl JsonlSessionLog {
    pub fn create(
        id: impl Into<String>,
        path: impl AsRef<Path>,
        bus: EventBus,
    ) -> Result<Arc<Self>, SessionError> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        // The crash tail is dropped from the file, not only from the reading.
        // Appending after a half-written record would splice the next event
        // onto it and leave one line no reader can parse.
        let scan = scan(&path)?;
        if scan.committed < file.metadata()?.len() {
            file.set_len(scan.committed)?;
            file.sync_all()?;
        }
        let events = scan.events;
        Ok(Arc::new(Self {
            id: id.into(),
            path,
            bus,
            state: Mutex::new(State { file, events }),
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write(
        &self,
        ty: &str,
        data: serde_json::Value,
        sources: Option<Vec<u64>>,
    ) -> Result<SessionEvent, SessionError> {
        let event = {
            let mut state = self.state.lock().expect("session lock");
            let event = SessionEvent {
                ty: ty.to_string(),
                seq: state.events.len() as u64,
                time: now_ms(),
                data,
                source_event_seqs: sources,
            };
            let line = serde_json::to_string(&event)
                .map_err(|_| SessionError::NotSerializable(ty.to_string()))?;
            writeln!(state.file, "{line}")?;
            state.file.sync_data()?;
            state.events.push(event.clone());
            event
        };
        self.bus.emit(&SessionEventDispatch {
            event: event.clone(),
        });
        Ok(event)
    }
}

impl SessionLog for JsonlSessionLog {
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
        self.state.lock().expect("session lock").events.clone()
    }

    fn flush(&self) -> Result<(), SessionError> {
        self.state.lock().expect("session lock").file.sync_all()?;
        Ok(())
    }
}

/// Write a whole journal at once, from events that already carry their `seq`
/// and `time`.
///
/// This is how a fork's seed is laid down (contract section 4.4.6): the copied
/// events keep the seqs and the times they were written under, which `append`
/// cannot do because it assigns both. The file must not exist. A seed written
/// onto a journal that already holds a history would splice two histories into
/// one file, and every seq after the join would name the wrong line.
///
/// The same rule the reader enforces is enforced here, so a seed cannot create
/// a journal `replay` would refuse: `seq` must equal the index of its line.
pub fn seed(path: impl AsRef<Path>, events: &[SessionEvent]) -> Result<(), SessionError> {
    let path = path.as_ref();
    for (i, event) in events.iter().enumerate() {
        if event.seq != i as u64 {
            return Err(SessionError::Corrupt(i + 1));
        }
    }
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    for event in events {
        let line = serde_json::to_string(event)
            .map_err(|_| SessionError::NotSerializable(event.ty.clone()))?;
        writeln!(file, "{line}")?;
    }
    // One barrier for the whole seed: unlike an append, no caller has been
    // told any part of this is durable until all of it is.
    file.sync_all()?;
    Ok(())
}

/// Read a journal back from disk. `seq` contiguity is verified: a gap means the
/// file is not a faithful copy of the log that produced it.
///
/// A record the writer did not finish is not a gap. See [`scan`] for which
/// damage is read past and which is refused.
pub fn replay(path: impl AsRef<Path>) -> Result<Vec<SessionEvent>, SessionError> {
    let events = scan(path.as_ref())?.events;
    for (i, event) in events.iter().enumerate() {
        if event.seq != i as u64 {
            return Err(SessionError::Corrupt(i + 1));
        }
    }
    Ok(events)
}

/// What one pass over a journal file found.
#[derive(Default)]
struct Scan {
    events: Vec<SessionEvent>,
    /// Bytes of the file that end on a record boundary: everything up to and
    /// including the last newline.
    committed: u64,
}

/// Read every committed record of a journal.
///
/// The newline is the commit. An append writes one record and fsyncs it, so
/// the only record a crash can cut short is the last one, and it is cut short
/// exactly when the file does not end in a newline. That tail is dropped: it
/// is a fact no `append` ever returned, so no caller was told it was durable.
///
/// Every other line must parse. A damaged line the writer *did* terminate is
/// not a crash tail - the log it came from is not the log on disk - so it is
/// refused rather than read past, and the caller is told which line it was.
fn scan(path: &Path) -> Result<Scan, SessionError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Scan::default()),
        Err(e) => return Err(e.into()),
    };
    let mut found = Scan::default();
    for (i, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let Some(record) = line.strip_suffix(b"\n") else {
            break;
        };
        found.committed += line.len() as u64;
        // A cut that lands inside a character leaves bytes that are no text
        // at all, so the record is read as bytes and judged as one line.
        let text = std::str::from_utf8(record)
            .map_err(|_| SessionError::Corrupt(i + 1))?
            .trim();
        if text.is_empty() {
            continue;
        }
        found
            .events
            .push(serde_json::from_str(text).map_err(|_| SessionError::Corrupt(i + 1))?);
    }
    Ok(found)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
