//! The live extension points of a turn: the waterfalls that decide what the
//! model sees, build the request, wrap the provider call and the tool pipeline,
//! plus the serial checkpoint that observes a turn stopping.
//!
//! Durable facts are not here - they are `SessionEvent`s on the log, broadcast
//! as `session/event`. Picking the right domain is the first decision in most
//! changes (upstream `docs/architecture.md`, "Events").

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
    pub fn text(&self) -> String {
        self.sections
            .iter()
            .map(|s| s.text.as_str())
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
    /// The turn never ended: this reason is written by crash repair when a
    /// later run finds the journal open. See [`crate::repair`].
    Interrupted,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Natural => "natural",
            StopReason::PreStepRejected => "pre-step-rejected",
            StopReason::MaxSteps => "max-steps",
            StopReason::Cancelled => "cancelled",
            StopReason::Interrupted => "interrupted",
        }
    }
}
