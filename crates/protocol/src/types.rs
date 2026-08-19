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
