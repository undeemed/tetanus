//! Shared fixture: one booted turn engine writing to a temporary journal, with
//! a [`TurnTrace`] attached so a test can read back the ordered event sequence
//! of one turn.

use std::path::PathBuf;
use std::sync::Arc;

use tempfile::TempDir;
use tetanus_core::EventBus;
use tetanus_session::{JsonlSessionLog, SessionLog};
use tetanus_turn::boot::boot;
use tetanus_turn::llm::mock::MockAdapter;
use tetanus_turn::tools::{EchoTool, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine, TurnTrace};

/// The complete documented sequence one mock turn emits: a first step that
/// calls a tool, a second step that answers, then the terminal checkpoint.
pub const MOCK_TURN_FLOW: &[&str] = &[
    "turn/start",
    // step 1: the model asks for a tool
    "agent/pre-step",
    "step/start",
    "user/message",
    "system-prompt/assemble",
    "agent/request",
    "llm/stream",
    "assistant/chunk",
    "assistant/chunk",
    "assistant/chunk",
    "assistant/chunk",
    "assistant/message",
    "tool/call",
    "tools/pre-execute",
    "tools/execute",
    "tools/post-execute",
    "tool/result",
    "step/end",
    // step 2: tools owed another request, so the driver claims and steps again
    "agent/pre-step",
    "step/start",
    "system-prompt/assemble",
    "agent/request",
    "llm/stream",
    "assistant/chunk",
    "assistant/chunk",
    "assistant/message",
    "step/end",
    "agent/turn-stopping",
    "turn/end",
];

pub struct Harness {
    pub engine: TurnEngine,
    pub log_path: PathBuf,
    trace: TurnTrace,
    bus: EventBus,
    _dir: TempDir,
}

impl Harness {
    pub async fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let log_path = dir.path().join(format!("{name}.jsonl"));

        let bus = EventBus::new();
        let log: Arc<dyn SessionLog> =
            JsonlSessionLog::create(name, &log_path, bus.clone()).expect("journal");
        let tools = Arc::new(ToolRegistry::new().with(Arc::new(EchoTool)));
        let ctx = boot(bus.clone(), Arc::new(MockAdapter::new()), tools, log).expect("boot");

        let trace = TurnTrace::attach(&bus);
        let engine = TurnEngine::from_context(&ctx, TurnConfig::default()).expect("engine");

        Self {
            engine,
            log_path,
            trace,
            bus,
            _dir: dir,
        }
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn trace(&self) -> Vec<String> {
        self.trace.topics()
    }
}
