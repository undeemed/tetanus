//! Shared fixture: one journal on disk, a registry over it, and the two ways a
//! case reads the state back.
//!
//! Every feature in this crate keeps its state on the journal, so every case
//! needs the same three things: a real `JsonlSessionLog`, a `ToolRegistry` to
//! dispatch through, and a replay from the file. A double for any of the three
//! would test the double - the promise under test is that a reload reproduces
//! what the run had.

#![allow(dead_code)]
// A test binary lints the parts of a shared fixture its own cases do not reach,
// and each suite here reaches a different part of this one.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tetanus_core::EventBus;
use tetanus_session::{JsonlSessionLog, SessionEvent, SessionLog};
use tetanus_turn::log::topic as turn_topic;
use tetanus_turn::tools::{Tool, ToolCall, ToolError, ToolOutcome, ToolRegistry, ToolSchema};

pub struct Fixture {
    log: Arc<dyn SessionLog>,
    path: PathBuf,
    /// The tools a case composed. A `ToolRegistry` is built from them per
    /// dispatch rather than held: the registry is settled by construction, and
    /// building one is a few map inserts, which is cheaper than the lifetime
    /// gymnastics of holding a lock across an await.
    tools: Mutex<Vec<Arc<dyn Tool>>>,
    bus: EventBus,
    _dir: TempDir,
}

impl Fixture {
    /// A journal with a turn open, which is the state a tool runs in.
    pub async fn new(name: &str) -> Self {
        let fixture = Self::bare(name);
        fixture.append(turn_topic::TURN_START, serde_json::json!({ "turn": 1 }));
        fixture
    }

    /// A journal with nothing on it, for the cases about a cold start.
    pub fn bare(name: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(format!("{name}.jsonl"));
        let bus = EventBus::new();
        let log: Arc<dyn SessionLog> =
            JsonlSessionLog::create(name, &path, bus.clone()).expect("journal");
        Self {
            log,
            path,
            tools: Mutex::new(Vec::new()),
            bus,
            _dir: dir,
        }
    }

    pub fn log(&self) -> Arc<dyn SessionLog> {
        Arc::clone(&self.log)
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn register(&self, tool: Arc<dyn Tool>) {
        self.tools.lock().expect("tools").push(tool);
    }

    /// The registry as it stands, for a dispatch or for a schema.
    fn registry(&self) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        for tool in self.tools.lock().expect("tools").iter() {
            registry.register(Arc::clone(tool));
        }
        registry
    }

    /// Dispatch one call the way the turn engine does, and answer what the
    /// model would read. A tool that failed the step panics here: the cases
    /// that are about failure use [`Self::dispatch`].
    pub async fn call(&self, name: &str, arguments: serde_json::Value) -> ToolOutcome {
        self.dispatch(name, arguments)
            .await
            .expect("the tool answered rather than failing the step")
    }

    /// Dispatch one call and hand back whatever came out, including a failure.
    pub async fn dispatch(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolOutcome, ToolError> {
        let call = ToolCall {
            id: format!("call-{name}"),
            name: name.to_string(),
            arguments,
        };
        // Built here, so no lock is held across the await: a tool body may
        // dispatch another call, and a fixture that deadlocked on itself would
        // read as the tool hanging.
        self.registry().execute(&call).await
    }

    pub fn schema(&self, name: &str) -> ToolSchema {
        self.registry()
            .schemas()
            .into_iter()
            .find(|schema| schema.name == name)
            .expect("the tool is registered")
    }

    pub fn append(&self, ty: &str, data: serde_json::Value) -> SessionEvent {
        self.log.append(ty, data).expect("append")
    }

    pub fn events(&self, ty: &str) -> Vec<SessionEvent> {
        self.log
            .events()
            .into_iter()
            .filter(|event| event.ty == ty)
            .collect()
    }

    pub fn flush(&self) {
        self.log.flush().expect("flush");
    }

    /// The journal as a cold reader sees it.
    pub fn replay(&self) -> Vec<SessionEvent> {
        tetanus_session::replay(&self.path).expect("replay")
    }
}
