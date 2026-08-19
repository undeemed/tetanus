//! Shared fixture: one booted turn engine writing to a temporary journal, with
//! a [`TurnTrace`] attached so a test can read back the ordered event sequence
//! of one turn.

use std::path::PathBuf;
use std::sync::Arc;

use tempfile::TempDir;
use tetanus_core::EventBus;
use tetanus_session::{JsonlSessionLog, SessionLog};
use tetanus_turn::boot::{boot, PromptService};
use tetanus_turn::llm::mock::MockAdapter;
use tetanus_turn::prompt::PromptRegistry;
use tetanus_turn::tools::{EchoTool, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine, TurnTrace};

/// The complete documented sequence one mock turn emits: a first step that
/// calls a tool, a second step that answers, then the terminal checkpoint.
// The turn-flow suite asserts against the whole sequence; the other suites
// sharing this fixture do not, and a test binary lints what it does not use.
#[allow(dead_code)]
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
    /// The prompt-section registry the engine assembles from. Only the
    /// system-prompt suite reaches for it, and a test binary lints the parts
    /// of a shared fixture it does not use.
    #[allow(dead_code)]
    pub sections: Arc<PromptRegistry>,
    trace: TurnTrace,
    // Not every suite sharing this fixture listens on the bus, and a test
    // binary lints the parts of it that its own cases do not reach.
    #[allow(dead_code)]
    bus: EventBus,
    _dir: TempDir,
}

impl Harness {
    #[allow(dead_code)]
    pub async fn new(name: &str) -> Self {
        Self::with_tools(name, ToolRegistry::new().with(Arc::new(EchoTool))).await
    }

    /// The same fixture with a caller-supplied registry, for cases about what
    /// the model is offered and what a call actually runs.
    pub async fn with_tools(name: &str, tools: ToolRegistry) -> Self {
        Self::with_config(name, tools, TurnConfig::default()).await
    }

    /// The same fixture with a caller-chosen turn config, for cases about a
    /// budget the default hides, such as the parallel tool-call cap.
    pub async fn with_config(name: &str, tools: ToolRegistry, config: TurnConfig) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let log_path = dir.path().join(format!("{name}.jsonl"));

        let bus = EventBus::new();
        let log: Arc<dyn SessionLog> =
            JsonlSessionLog::create(name, &log_path, bus.clone()).expect("journal");
        let ctx = boot(
            bus.clone(),
            Arc::new(MockAdapter::new()),
            Arc::new(tools),
            log,
        )
        .expect("boot");

        let trace = TurnTrace::attach(&bus);
        let sections = ctx.services.require::<PromptService>().expect("sections");
        let engine = TurnEngine::from_context(&ctx, config).expect("engine");

        Self {
            engine,
            log_path,
            sections,
            trace,
            bus,
            _dir: dir,
        }
    }

    #[allow(dead_code)]
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn trace(&self) -> Vec<String> {
        self.trace.topics()
    }
}
