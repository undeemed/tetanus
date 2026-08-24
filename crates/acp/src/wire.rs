//! The Agent Client Protocol's own vocabulary, as Rust.
//!
//! ACP is camelCase on the wire and this workspace's contract is snake_case,
//! which is the first reason the two vocabularies are separate types rather
//! than one with two serializations: a shared type would have to pick, and
//! whichever it picked would silently reshape the other protocol.
//!
//! The second reason is that they are different protocols with different
//! owners. ACP's `stopReason` is a closed set of five words agreed by other
//! implementations; `tetanus_protocol::types::StopReason` is this workspace's
//! and grows when this workspace needs it to. Mapping between them is
//! [`StopReason::of`], one function, so the place where the two disagree is a
//! place a reader can find.

use serde::{Deserialize, Serialize};

/// The ACP major version this bridge speaks.
///
/// A single-version agent: the specification's "answer with the client's
/// version if you support it, otherwise the latest you do" collapses to this
/// number either way.
pub const PROTOCOL_VERSION: u32 = 1;

/// The name this bridge introduces itself with.
pub const AGENT_NAME: &str = "tetanus-acp";

/// Client-to-agent method names.
pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const AUTHENTICATE: &str = "authenticate";
    pub const SESSION_NEW: &str = "session/new";
    /// Re-open a session this agent already has a journal for, replaying its
    /// history to the client before answering.
    pub const SESSION_LOAD: &str = "session/load";
    pub const SESSION_PROMPT: &str = "session/prompt";
    /// A notification, not a call: ACP's cancel is one-way.
    pub const SESSION_CANCEL: &str = "session/cancel";
}

/// Agent-to-client frames.
pub mod agent {
    /// A one-way notification carrying one [`super::SessionUpdate`].
    pub const SESSION_UPDATE: &str = "session/update";
    /// A request the client answers with an outcome.
    pub const REQUEST_PERMISSION: &str = "session/request_permission";
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    #[serde(default)]
    pub protocol_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_capabilities: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u32,
    pub agent_info: AgentInfo,
    pub agent_capabilities: AgentCapabilities,
    /// Empty: this bridge advertises no authentication method, which is what
    /// makes `authenticate` a no-op rather than a refusal.
    pub auth_methods: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// Whether `session/load` is served. True here: the journal is the whole
    /// truth of a session and is already replayable, so re-opening one costs a
    /// paged read rather than a store this bridge would have to keep.
    pub load_session: bool,
    pub prompt_capabilities: PromptCapabilities,
}

/// What a prompt may contain.
///
/// All three are false, and each for its own reason rather than as a blanket
/// stance. The blocker for an image or an embedded resource is *not* storage -
/// `crates/features` has had a content-addressed attachment store since the
/// feature-tools slice landed - it is that the model-visible message this
/// workspace sends is `tetanus_turn::llm::Message`, whose `content` is a
/// `String`. Until that seam carries parts rather than a string, an admitted
/// image has nowhere to go in the request, and the durable half would be
/// storing bytes no turn can refer to. Audio has no model behind it here at
/// all. Advertising a capability the bridge cannot honour would move the
/// failure from `initialize`, where a client can adapt, to the middle of a
/// prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    pub image: bool,
    pub audio: bool,
    pub embedded_context: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionRequest {
    /// The workspace root, which ACP requires to be absolute.
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub mcp_servers: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: String,
}

/// Re-open an existing session.
///
/// The same shape as [`NewSessionRequest`] with the id of the session to
/// re-open. ACP answers this with an empty result: what the client is really
/// buying is the `session/update` replay that precedes the answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionRequest {
    #[serde(default)]
    pub session_id: String,
    /// The workspace root, which ACP requires to be absolute here too.
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub mcp_servers: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub session_id: String,
    #[serde(default)]
    pub prompt: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelNotification {
    pub session_id: String,
}

/// One block of a prompt, or of an agent message.
///
/// `Other` is the growth path. ACP adds block kinds, and a bridge that failed
/// to *parse* an unknown one could not tell a client which kind it was
/// refusing; parsing it and refusing it by name can.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Image {
        data: String,
        mime_type: String,
    },
    #[serde(rename_all = "camelCase")]
    Audio {
        data: String,
        mime_type: String,
    },
    #[serde(rename_all = "camelCase")]
    ResourceLink {
        name: String,
        uri: String,
    },
    Resource {
        resource: serde_json::Value,
    },
    #[serde(untagged)]
    Other(serde_json::Value),
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// The word this bridge uses for the block when refusing it.
    pub fn kind(&self) -> &str {
        match self {
            Self::Text { .. } => "text",
            Self::Image { .. } => "image",
            Self::Audio { .. } => "audio",
            Self::ResourceLink { .. } => "resource_link",
            Self::Resource { .. } => "resource",
            Self::Other(value) => value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown"),
        }
    }
}

/// Why a prompt stopped, in ACP's closed vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

impl StopReason {
    /// The closest legal ACP reason for one of this workspace's.
    ///
    /// Two mappings are worth their words. `MaxSteps` becomes
    /// `max_turn_requests` and not `max_tokens`: what ran out was the driver's
    /// budget of model requests, and a client that saw `max_tokens` would
    /// retry with a shorter prompt and be wrong about why.
    ///
    /// Everything else becomes `end_turn`, including a turn a pre-step
    /// listener rejected. `refusal` in ACP is the *model* declining, which is a
    /// different fact from the harness declining, and reporting a harness
    /// decision as a model refusal would put words in the model's mouth. The
    /// reason is on the journal for anyone who wants it.
    pub fn of(reason: &tetanus_protocol::types::StopReason) -> Self {
        use tetanus_protocol::types::StopReason as Harness;
        match reason {
            Harness::Natural => Self::EndTurn,
            Harness::MaxSteps => Self::MaxTurnRequests,
            Harness::Cancelled => Self::Cancelled,
            Harness::PreStepRejected => Self::EndTurn,
            // Section 7.5's fallback. A word this build does not know is not a
            // cancellation and not a refusal, so it is ordinary quiescence.
            Harness::Other(_) => Self::EndTurn,
        }
    }
}

/// The payload of a `session/update` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    pub session_id: String,
    pub update: SessionUpdate,
}

/// One thing that happened, in ACP's words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    #[serde(rename_all = "camelCase")]
    AgentMessageChunk { content: ContentBlock },
    #[serde(rename_all = "camelCase")]
    ToolCall {
        tool_call_id: String,
        title: String,
        status: ToolCallStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw_input: Option<serde_json::Value>,
    },
    #[serde(rename_all = "camelCase")]
    ToolCallUpdate {
        tool_call_id: String,
        status: ToolCallStatus,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<ToolCallContent>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCallContent {
    Content { content: ContentBlock },
}

/// Params of the agent-to-client `session/request_permission` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionRequest {
    pub session_id: String,
    pub tool_call: PermissionToolCall,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionToolCall {
    pub tool_call_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

/// The two choices this bridge offers, and nothing else.
///
/// One-shot only, and deliberately: ACP lets an agent offer "always allow", and
/// a client answering it would be setting a durable policy through a channel
/// that has no way to audit or revoke one. Contract section 4.4.7 gives
/// `AllowedOnce` the same treatment - "it grants the one call it was asked
/// about: it is not a rule" - so offering more here would let the bridge
/// promise something the engine underneath does not implement.
pub const ALLOW_ONCE: &str = "allow-once";
pub const REJECT_ONCE: &str = "reject-once";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResponse {
    pub outcome: PermissionOutcome,
}

/// How the client answered. `Selected` names one of the offered options;
/// `Cancelled` is the client withdrawing the question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PermissionOutcome {
    Cancelled,
    #[serde(rename_all = "camelCase")]
    Selected {
        option_id: String,
    },
}
