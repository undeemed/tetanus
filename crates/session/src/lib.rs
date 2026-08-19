//! Session log: the append-only journal of durable `SessionEvent`s that is the
//! source of the context a model sees. Model history is *derived* from this log,
//! never stored beside it, so replay is re-derivation from the same events.
//!
//! Shape parity with upstream (`docs/subsystems/session.md`, the
//! `SessionEvent<T>` log entry): a discriminated union over `type`, a `seq`
//! equal to the log length at append time, `time` in epoch milliseconds, a JSON
//! `data` payload, and - on the three surface event types - the
//! `sourceEventSeqs` an event cites.

use std::io::{BufRead, Write};
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
        let events = read_jsonl(&path)?;
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

/// Read a journal back from disk. `seq` contiguity is verified: a gap means the
/// file is not a faithful copy of the log that produced it.
pub fn replay(path: impl AsRef<Path>) -> Result<Vec<SessionEvent>, SessionError> {
    let events = read_jsonl(path.as_ref())?;
    for (i, event) in events.iter().enumerate() {
        if event.seq != i as u64 {
            return Err(SessionError::Corrupt(i + 1));
        }
    }
    Ok(events)
}

fn read_jsonl(path: &Path) -> Result<Vec<SessionEvent>, SessionError> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for (i, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&line).map_err(|_| SessionError::Corrupt(i + 1))?);
    }
    Ok(out)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
