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
use crate::prompt::{interpolate, PromptError, Variables};
use crate::tools::{ToolCall, ToolError, ToolOutcome, ToolSchema};

/// One named piece of the system prompt. Plugins contribute sections; the
/// engine never hard-codes prompt text beyond the base section.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PromptSection {
    pub id: String,
    pub text: String,
}

/// The assembled prompt: ordered sections, the tool schemas the model may call
/// this step, and the variables those sections may name.
///
/// Section text is carried as it was contributed, references and all. It
/// becomes the text the model reads at [`render`](Self::render), which is the
/// last thing that happens to it, so a listener that rewrites a section or
/// adds a variable is writing into the same assembly the substitution reads.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct SystemPrompt {
    pub sections: Vec<PromptSection>,
    pub tools: Vec<ToolSchema>,
    /// Every name a section may reference, with the value its provider gave
    /// for this assembly. A name that is absent is registered nowhere; a name
    /// present with no value is registered and had nothing to say this time.
    pub variables: Variables,
}

impl SystemPrompt {
    /// The model-facing text: every section substituted, then every section
    /// that has any text, in order, separated by a blank line.
    ///
    /// An empty section contributes nothing at all. A deployment that
    /// configures no persona does not pay a blank gap for the section it left
    /// unfilled, and an assembly whose sections are all empty renders as the
    /// empty string, which is what keeps the system message off the request
    /// entirely. A section is measured after substitution, so one whose only
    /// content is a variable with an empty value drops out too.
    ///
    /// Substitution is strict, and a section that names a variable this
    /// assembly cannot give it fails the render rather than reaching the model
    /// as prose it would have read as an instruction. [`interpolate`] says
    /// which mistakes those are.
    ///
    /// Parity: upstream `renderPrompt`, `packages/core/system-prompt/src`,
    /// "interpolate strict `{{variable}}` references, drop empty sections, and
    /// join the rest with blank lines".
    pub fn render(&self) -> Result<String, PromptError> {
        let mut rendered = Vec::with_capacity(self.sections.len());
        for section in &self.sections {
            let text = interpolate(&section.text, &section.id, &self.variables)?;
            if !text.is_empty() {
                rendered.push(text);
            }
        }
        Ok(rendered.join("\n\n"))
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
    /// The registry's variables, resolved for this assembly. A listener may
    /// add a name or replace a value, and the render reads what it left.
    pub variables: Variables,
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
    /// The provider's own id for the refused request, from
    /// [`LlmError::request_id`].
    ///
    /// Carried to a listener because that is where it becomes useful: what a
    /// person does with a refusal they cannot explain is quote this to the
    /// provider, and by the time the failure reaches a log line the response
    /// it came on is long gone.
    pub provider_request_id: Option<String>,
}

impl From<&LlmError> for RequestFailure {
    fn from(error: &LlmError) -> Self {
        Self {
            code: error.code().to_string(),
            message: error.to_string(),
            provider_retry_after_ms: error.retry_after_ms(),
            provider_request_id: error.request_id().map(str::to_string),
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

/// What a `tools/post-execute` listener leaves for the loop.
///
/// Two things, because a listener has two different things to say. The
/// `outcome` is what the *model* reads as this call's result. The contexts are
/// what the loop should put in front of the model *next*, which is not the
/// same thing and must not be smuggled into the result: a guard that appended
/// "you have called this five times" to a tool's output would be corrupting
/// the tool's answer to make a point about the caller, and a tool author
/// parsing that output back would find a sentence nobody wrote.
///
/// Parity: upstream's `PostToolDecision.additionalContexts`, which its
/// repeat-tool guard and its hook bridges both write and nothing else reads.
#[derive(Debug, Clone, PartialEq)]
pub struct PostToolDecision {
    pub outcome: ToolOutcome,
    /// Messages to deliver at the next step boundary, in the order given.
    ///
    /// They ride the *decision* rather than being appended by the listener,
    /// so a call whose result is never committed - one behind an earlier
    /// call's fault - contributes no context either. A listener that wrote
    /// straight to the journal could not be held to that.
    pub additional_contexts: Vec<crate::llm::Message>,
}

impl PostToolDecision {
    /// The decision that changes nothing: this outcome, no context.
    pub fn keep(outcome: ToolOutcome) -> Self {
        Self {
            outcome,
            additional_contexts: Vec::new(),
        }
    }

    /// Put one message in front of the model at the next boundary, keeping
    /// whatever a later listener already asked for.
    pub fn with_context(mut self, message: crate::llm::Message) -> Self {
        self.additional_contexts.push(message);
        self
    }
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
    type Output = PostToolDecision;
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
