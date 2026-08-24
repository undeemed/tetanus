//! The domain vocabulary both lanes share. These are *wire* shapes: the engine
//! converts its internal types into them, so an engine refactor is not a
//! breaking change and a presentation surface never depends on an engine crate.
//!
//! Enums that may grow carry an `Other(String)` fallback. A presentation
//! surface must render the fallback rather than fail, which is what lets the
//! engine add a variant in a minor version.

use serde::{Deserialize, Serialize};

/// One durable fact on a session journal, exactly as the log stores it.
///
/// `ty` stays a free string: the durable vocabulary grows (`todo/write`,
/// `fs/observed`, …) and a presentation surface must pass unknown types
/// through instead of dropping them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    #[serde(rename = "type")]
    pub ty: String,
    /// Position in the journal. `seq` equals the index of its line, so a
    /// replay verifies contiguity.
    pub seq: u64,
    /// Unix epoch milliseconds.
    pub time: u64,
    pub data: serde_json::Value,
    /// Earlier events this one was assembled from. Present only on surface
    /// events; an `assistant/message` may cite a known-empty list.
    #[serde(rename = "sourceEventSeqs", skip_serializing_if = "Option::is_none")]
    pub source_event_seqs: Option<Vec<u64>>,
}

/// Everything a surface needs to list or open a session without reading its
/// journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    /// Absolute path of the JSONL journal.
    pub path: String,
    pub provider: String,
    pub model: String,
    pub created_time: u64,
    /// Seq of the last event on the journal; `-1` for an empty log.
    pub last_seq: i64,
    /// The session's first user message, truncated by the engine. `None`
    /// until one exists. A picker that had to page every journal for this
    /// line would be reading the engine's side of the boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state: AgentState,
}

/// Live agent state. Not durable, and not derivable from the journal while a
/// turn is in flight, which is why it is pushed as `agent/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentState {
    /// Nothing is owed; the session accepts a prompt.
    Idle,
    /// A turn is in flight.
    Running,
    #[serde(untagged)]
    Other(String),
}

/// Why a turn closed. Mirrors the engine's `StopReason`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopReason {
    Natural,
    PreStepRejected,
    MaxSteps,
    Cancelled,
    #[serde(untagged)]
    Other(String),
}

/// The closing summary of one turn. Every field is also reconstructable from
/// the journal; this is the convenience form a surface renders directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnSummary {
    pub turn: u64,
    pub steps: u32,
    pub stop_reason: StopReason,
    /// Set when an `agent/turn-stopping` listener held the turn open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_veto: Option<String>,
    /// The last assistant message of the turn.
    pub content: String,
    /// Wall clock for the whole turn. Derivable from `SessionEvent.time`;
    /// this is only the engine saying it once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Tokens the turn spent. Derivable from nothing else on this boundary.
    /// `None` means this build did not measure it, never zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// Tokens one request spent, in the provider's own words. The same object is
/// what `assistant/message.usage` carries in the journal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// One tool as the model sees it, and as a help surface lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
}

/// One model provider and the models it advertises.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    /// The provider route, e.g. `mock` or `deepseek-official`.
    pub provider: String,
    /// Advisory catalog. An unlisted model id still passes through.
    pub models: Vec<String>,
    /// Environment variable holding the credential, when the provider needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_env: Option<String>,
    /// False when the provider is registered but its credential is absent.
    pub available: bool,
}

/// Which layer settled a config key. Ordered weakest to strongest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigLayer {
    Default,
    File,
    Env,
    Flag,
    #[serde(untagged)]
    Other(String),
}

/// What stands in a [`ConfigEntry`] where a secret would be.
///
/// The engine never publishes a credential it read out of the settings
/// document; the entry stays, so a surface can still say the key is set and
/// where it was set, and this is what its value reads. Section 4.3 of
/// `docs/interface-contract.md` names which keys it applies to.
pub const REDACTED: &str = "<redacted>";

/// One resolved config key with its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigEntry {
    pub key: String,
    pub value: serde_json::Value,
    pub layer: ConfigLayer,
}

/// One selectable answer offered to the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// User-facing text, and also the value the answer carries back.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One question in an ask. `id` is echoed in the answer, so a batch stays
/// routable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub question: String,
    /// Supporting text rendered with the question, kept out of option labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<QuestionOption>,
    /// Default is single-select.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
}

/// The answer to one [`Question`], carrying the labels the user chose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub id: String,
    pub labels: Vec<String>,
}

/// How one approval question settled.
///
/// Contract section 4.4.7. [`AllowedOnce`](Self::AllowedOnce) is the only
/// grant, and it grants the one call it was asked about: it is not a rule, not
/// a session setting, and the next call of the same tool asks again.
///
/// [`Other`](Self::Other) is section 7.5's fallback, and this is the first
/// growable enum whose fallback the engine *reads* rather than renders, so
/// what reading it means is fixed rather than left to a caller: a word the
/// engine cannot interpret is not a grant, so it denies exactly as
/// [`Unavailable`](Self::Unavailable) does. That keeps an added variant a
/// minor change without letting an unknown one open a gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalOutcome {
    /// Run this call, and only this call.
    AllowedOnce,
    /// A decision not to run it.
    Rejected,
    /// The question was withdrawn before it was answered.
    Cancelled,
    /// Nobody could answer it. The fail-closed outcome.
    Unavailable,
    #[serde(untagged)]
    Other(String),
}

impl ApprovalOutcome {
    /// Whether this outcome lets the call run.
    ///
    /// Only [`AllowedOnce`](Self::AllowedOnce) does. Every other value denies,
    /// [`Other`](Self::Other) included, which is the fail-closed rule as one
    /// function rather than as a match every caller writes for itself.
    pub fn grants(&self) -> bool {
        matches!(self, Self::AllowedOnce)
    }
}

/// What happens to an approval question before any client sees it.
///
/// Contract section 4.4.7. The session's policy is the last `approval/policy`
/// on its journal, and the deployment's `approval.policy` setting when the
/// journal holds none, so a resumed session is under the policy it was under
/// with nothing to replay but the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalPolicy {
    /// Put the question to the client.
    Ask,
    /// Put it to nobody: every ask settles [`ApprovalOutcome::Rejected`]. The
    /// unattended stance, whose point is that the answer is knowable without a
    /// human, so a run in CI neither hangs nor waits on a client.
    Never,
    /// A word this build does not know. Unlike [`ApprovalOutcome::Other`] this
    /// one is never acted on: a policy is set by a caller that could have
    /// named one of the two, so the engine answers `InvalidParams`.
    #[serde(untagged)]
    Other(String),
}

/// One tool call as the model asked for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// One piece of a provider stream, as `assistant/chunk` records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "chunk", rename_all = "snake_case")]
pub enum Chunk {
    /// Visible assistant text.
    Text { delta: String },
    /// Thinking-mode text. Model-visible, and kept out of derived history.
    Reasoning { delta: String },
    /// A tool call the provider finished assembling.
    ToolCall { call: ToolCall },
}

/// The payload of a durable type this contract version knows, per section
/// 4.3.1. Parsing is a fast path for the known types, not a closed
/// vocabulary: an unknown type is still rendered from the raw event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum KnownEvent {
    #[serde(rename = "session/start")]
    SessionStart {
        session_id: String,
        provider: String,
        model: String,
        max_steps: u32,
        /// The session this journal was forked from, absent on one that was
        /// opened rather than forked (section 4.4.6).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
        /// The last parent seq this journal inherited, inclusive. Present
        /// exactly when `parent_session` is: the inherited prefix is seqs `1`
        /// through this one, and the child's own first event is the one after
        /// it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork_seq: Option<u64>,
        /// The working directory the session was opened in (section 4.4.9).
        ///
        /// Where it was opened, never where it is now: a tool may change
        /// directory and this header is not rewritten. It is recorded because
        /// a journal full of relative paths cannot be read without it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// The session that started this one as a subagent (section 4.4.9).
        ///
        /// Deliberately not `parent_session`. A fork is a *copy* and begins
        /// holding another journal's history; a subagent is a different
        /// conversation that another one asked for and shares no history. A
        /// session may carry both, which is why they are two fields rather
        /// than one field with a kind beside it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spawned_by: Option<String>,
        /// How many levels of delegation deep this session is; absent means
        /// none (section 4.4.9).
        ///
        /// Durable rather than held in memory because the bound on delegation
        /// has to survive a resume: a subagent whose harness restarted must
        /// not come back believing it is a root session and free to delegate
        /// again.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        depth: Option<u32>,
    },
    #[serde(rename = "turn/start")]
    TurnStart { turn: u64 },
    #[serde(rename = "step/start")]
    StepStart { turn: u64, step: u32 },
    #[serde(rename = "user/message")]
    UserMessage { content: String },
    #[serde(rename = "assistant/chunk")]
    AssistantChunk {
        #[serde(flatten)]
        chunk: Chunk,
        turn: u64,
        step: u32,
    },
    #[serde(rename = "assistant/message")]
    AssistantMessage {
        content: String,
        #[serde(default)]
        reasoning: String,
        #[serde(default)]
        tool_calls: Vec<ToolCall>,
        #[serde(default)]
        finish_reason: Option<String>,
        #[serde(default)]
        usage: Option<Usage>,
    },
    #[serde(rename = "tool/call")]
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    #[serde(rename = "tool/result")]
    ToolResult {
        /// The `tool/call.id` that asked for this. A surface pairs a result to
        /// its call by this id, never by arrival order.
        call_id: String,
        name: String,
        ok: bool,
        content: String,
    },
    #[serde(rename = "step/end")]
    StepEnd { turn: u64, step: u32 },
    #[serde(rename = "turn/end")]
    TurnEnd {
        turn: u64,
        steps: u32,
        stop_reason: StopReason,
        #[serde(default)]
        stop_veto: Option<String>,
    },
}

impl SessionEvent {
    /// The payload as section 4.3.1 fixes it, or `None` for a type this build
    /// does not know or a payload that does not match. The caller renders the
    /// raw event either way; this only removes the guesswork for the types
    /// the contract has pinned down.
    pub fn parse(&self) -> Option<KnownEvent> {
        let mut tagged = self.data.clone();
        tagged.as_object_mut()?.insert(
            "type".to_string(),
            serde_json::Value::String(self.ty.clone()),
        );
        serde_json::from_value(tagged).ok()
    }
}
