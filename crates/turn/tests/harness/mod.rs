//! Shared fixture: one booted turn engine writing to a temporary journal, with
//! a [`TurnTrace`] attached so a test can read back the ordered event sequence
//! of one turn.

use std::path::PathBuf;
use std::sync::Arc;

use tempfile::TempDir;
use tetanus_core::EventBus;
use tetanus_session::{JsonlSessionLog, SessionLog};
use tetanus_turn::boot::{boot, ContextService, PromptService};
use tetanus_turn::llm::mock::MockAdapter;
use tetanus_turn::prompt::PromptRegistry;
use tetanus_turn::runtime_context::ContextRegistry;
use tetanus_turn::tools::{EchoTool, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine, TurnTrace};

/// The complete documented sequence one mock turn emits: a first step that
/// calls a tool, a second step that answers, then the terminal checkpoint.
///
/// `request/context` joined the sequence with the context lane: it is the
/// request envelope - the route, its window, and what the system prompt and
/// tool catalog cost - written before the request rather than after the
/// answer, so a turn a provider failure ended still says what it tried to
/// send. It sits after `system-prompt/assemble`, because it prices what that
/// assembly produced, and before `agent/request`, because a listener that
/// rewrites the request must not be able to change what the journal already
/// said the request was. It is the anchor `context.breakdown` folds
/// (`crates/turn/src/projections.rs`) and the record that gave the three token
/// projections the envelope `docs/parity.md` named as their blocker.
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
    "request/context",
    "agent/request",
    "llm/stream",
    "assistant/chunk",
    "assistant/chunk",
    "assistant/chunk",
    "assistant/chunk",
    "assistant/message",
    "tool/call",
    "tools/pre-execute",
    // Every call is routed past the permission seam now, not only the ones the
    // registry pre-declares: a gate a deployment can only reach for calls
    // somebody already flagged cannot be added to an existing tool.
    //
    // After `tools/pre-execute` and not before it, which is the same rule the
    // approval gate already followed: a listener may rewrite the call, and
    // what is decided has to be what would actually run. Deciding first would
    // permit one command and execute another.
    "tools/permission",
    "tools/execute",
    "tools/post-execute",
    "tool/result",
    "step/end",
    // step 2: tools owed another request, so the driver claims and steps again
    "agent/pre-step",
    "step/start",
    "system-prompt/assemble",
    "request/context",
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
    /// Where the journal is on disk, for the suites that replay it rather than
    /// read the live log. A test binary lints the parts of a shared fixture its
    /// own cases do not reach.
    #[allow(dead_code)]
    pub log_path: PathBuf,
    /// The prompt-section registry the engine assembles from. Only the
    /// system-prompt suite reaches for it, and a test binary lints the parts
    /// of a shared fixture it does not use.
    #[allow(dead_code)]
    pub sections: Arc<PromptRegistry>,
    /// The runtime-context providers this engine will ask, registered on after
    /// boot. Only the runtime-context suite reaches for it.
    #[allow(dead_code)]
    pub context: Arc<ContextRegistry>,
    #[allow(dead_code)]
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
        let context = ctx.services.require::<ContextService>().expect("context");
        let engine = TurnEngine::from_context(&ctx, config).expect("engine");

        Self {
            engine,
            log_path,
            sections,
            context,
            trace,
            bus,
            _dir: dir,
        }
    }

    #[allow(dead_code)]
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    #[allow(dead_code)]
    pub fn trace(&self) -> Vec<String> {
        self.trace.topics()
    }
}
