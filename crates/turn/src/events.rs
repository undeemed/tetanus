//! The live extension points of a turn: the waterfalls that decide what the
//! model sees, build the request, wrap the provider call and the tool pipeline,
//! plus the serial checkpoint that observes a turn stopping.
//!
//! Durable facts are not here - they are `SessionEvent`s on the log, broadcast
//! as `session/event`. Picking the right domain is the first decision in most
//! changes (upstream `docs/architecture.md`, "Events").

use std::sync::Arc;

use tetanus_core::events::{DispatchMode, Event};

use crate::llm::{ChunkSink, LlmError, ModelRequest, ModelResponse};
use crate::tools::{ToolCall, ToolError, ToolOutcome, ToolSchema};

/// One named piece of the system prompt. Plugins contribute sections; the
/// engine never hard-codes prompt text beyond the base section.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PromptSection {
    pub id: String,
    pub text: String,
}

/// The assembled prompt: ordered sections plus the tool schemas the model may
/// call this step.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct SystemPrompt {
    pub sections: Vec<PromptSection>,
    pub tools: Vec<ToolSchema>,
}

impl SystemPrompt {
    /// The model-facing text: every section that has any, in order, separated
    /// by a blank line.
    ///
    /// An empty section contributes nothing at all. A deployment that
    /// configures no persona does not pay a blank gap for the section it left
    /// unfilled, and an assembly whose sections are all empty renders as the
    /// empty string, which is what keeps the system message off the request
    /// entirely.
    ///
    /// Parity: upstream `renderPrompt`, `packages/core/system-prompt/src`,
    /// "drop empty sections, and join the rest with blank lines".
    pub fn text(&self) -> String {
        self.sections
            .iter()
            .map(|s| s.text.as_str())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// `agent/pre-step` decides what the model sees. A listener may rewrite the
/// claimed messages or reject them outright; a rejected or empty first claim
/// still closes a durable turn that spent no step, so the log records the
/// attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum PreStepDecision {
    Enter(Vec<crate::llm::Message>),
    Reject(String),
}

pub struct PreStep {
    pub turn: u64,
    pub step: u32,
    /// The claimed batch, as the engine proposes it.
    pub messages: Vec<crate::llm::Message>,
}
impl Event for PreStep {
    const TOPIC: &'static str = "agent/pre-step";
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Output = PreStepDecision;
}

pub struct AssemblePrompt {
    pub turn: u64,
    pub step: u32,
    pub sections: Vec<PromptSection>,
    pub tools: Vec<ToolSchema>,
}
impl Event for AssemblePrompt {
    const TOPIC: &'static str = "system-prompt/assemble";
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Output = SystemPrompt;
}

pub struct AgentRequest {
    pub turn: u64,
    pub step: u32,
    pub request: ModelRequest,
}
impl Event for AgentRequest {
    const TOPIC: &'static str = "agent/request";
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Output = ModelRequest;
}

/// `llm/stream` wraps exactly one provider call. The terminal is the resolved
/// adapter; a listener may replace, retry around, or record the stream.
pub struct LlmStream {
    pub request: ModelRequest,
    pub sink: Box<dyn ChunkSink>,
}
impl Event for LlmStream {
    const TOPIC: &'static str = "llm/stream";
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Output = Result<ModelResponse, LlmError>;
}

/// One failed model request, as a recovery policy sees it.
///
/// It carries the failure's stable classification and its words rather than
/// the [`LlmError`] itself, so a listener written against this event keeps
/// compiling on the day the error enum grows a variant. Upstream passes the
/// same three fields as its `LlmFailure`.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestFailure {
    /// The stable failure code, from [`LlmError::code`].
    pub code: String,
    /// The provider's own words, or the transport's.
    pub message: String,
    /// The wait the provider asked for, in milliseconds, when it asked for
    /// one, from [`LlmError::retry_after_ms`]. A failure that asked for
    /// nothing leaves a policy on its own backoff.
    pub provider_retry_after_ms: Option<f64>,
}

impl From<&LlmError> for RequestFailure {
    fn from(error: &LlmError) -> Self {
        Self {
            code: error.code().to_string(),
            message: error.to_string(),
            provider_retry_after_ms: error.retry_after_ms(),
        }
    }
}

/// What a listener asks the driver to do about a failed model request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestErrorAction {
    /// Send the same request again. The failure is not reported.
    Retry,
}

/// `agent/request-error` is the recovery extension point: the provider call
/// failed, and a listener may ask for another attempt before the turn is
/// failed with it. The terminal recovers nothing, so a bus with no listener
/// behaves exactly as it did before - the failure ends the turn.
///
/// The event returns a decision and not a delay: any waiting is the
/// listener's, which is what keeps the driver free of a clock and lets a
/// policy record its own wait durably before serving it.
///
/// Parity: upstream's event of the same name, which its `llm-retry` package
/// hooks (`packages/llm/llm-retry/src/index.ts`). tetanus's executor is
/// [`crate::llm::retry::install`].
pub struct RequestError {
    pub turn: u64,
    pub step: u32,
    /// The route that failed, so a policy scoped to one provider can tell
    /// whether the failure is its business.
    pub provider: String,
    pub failure: RequestFailure,
    /// The interrupt this turn watches. A listener that waits before it
    /// answers waits on this, so a caller who asks the turn to stop is not
    /// held up by a backoff nobody is waiting for any more.
    pub interrupt: Arc<crate::interrupt::Interrupt>,
}
impl Event for RequestError {
    const TOPIC: &'static str = "agent/request-error";
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Output = Option<RequestErrorAction>;
}

/// Hooks, permission and sandbox policy run here, before the call starts.
pub struct ToolsPreExecute {
    pub turn: u64,
    pub call: ToolCall,
}
impl Event for ToolsPreExecute {
    const TOPIC: &'static str = "tools/pre-execute";
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Output = ToolCall;
}

/// Around-dispatch concerns - timeout, retry, metrics - wrap the call itself.
pub struct ToolsExecute {
    pub turn: u64,
    pub call: ToolCall,
}
impl Event for ToolsExecute {
    const TOPIC: &'static str = "tools/execute";
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Output = Result<ToolOutcome, ToolError>;
}

/// Accept, block, replace, or add context to the result before it is logged.
pub struct ToolsPostExecute {
    pub turn: u64,
    pub call: ToolCall,
    pub outcome: ToolOutcome,
}
impl Event for ToolsPostExecute {
    const TOPIC: &'static str = "tools/post-execute";
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Output = ToolOutcome;
}

/// A listener bails here to hold a turn open. Phase ① records the veto on
/// `turn/end`; re-entering the loop from it is Phase ② (continuation).
#[derive(Debug, Clone, PartialEq)]
pub struct TurnStopVeto {
    pub reason: String,
}

/// The serial terminal checkpoint. It has no `next()`: listeners observe in
/// order and the first bail wins.
pub struct TurnStopping {
    pub turn: u64,
    pub steps: u32,
    pub reason: StopReason,
}
impl Event for TurnStopping {
    const TOPIC: &'static str = "agent/turn-stopping";
    const MODE: DispatchMode = DispatchMode::Serial;
    type Output = TurnStopVeto;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopReason {
    /// The model asked for no more tools and nothing is owed.
    Natural,
    /// `agent/pre-step` rejected the claim, or rewrote a first claim empty.
    PreStepRejected,
    /// The step budget ran out before the model settled.
    MaxSteps,
    /// `TurnEngine::cancel` asked the turn to stop at a step boundary.
    Cancelled,
    /// The model reached the cap on what it may write, so the answer stops
    /// mid-write rather than because it was finished
    /// (`docs/interface-contract.md` section 4.4.2).
    MaxTokens,
    /// The turn never ended: this reason is written by crash repair when a
    /// later run finds the journal open. See [`crate::repair`].
    Interrupted,
}

/// The `turn/end` reason for a turn a failure ended
/// (`docs/interface-contract.md` section 4.4.2).
///
/// A value on the journal rather than a [`StopReason`] variant, because no
/// value of the enum could carry it: a turn that failed answers with its
/// failure and produces no [`crate::TurnOutcome`], so this reason is only ever
/// read off the event. Section 7.5 of the contract makes it a value of the
/// growable wire enum, which is where a surface meets it.
pub const FAILED_STOP_REASON: &str = "failed";

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Natural => "natural",
            StopReason::PreStepRejected => "pre-step-rejected",
            StopReason::MaxSteps => "max-steps",
            StopReason::Cancelled => "cancelled",
            StopReason::MaxTokens => "max-tokens",
            StopReason::Interrupted => "interrupted",
        }
    }
}
