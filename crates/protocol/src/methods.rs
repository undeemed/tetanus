//! Every call the contract defines: its name, its params, its result, and the
//! facade both surfaces drive. One method per call, so the RPC server and the
//! in-process CLI client cannot diverge.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::rpc::{ErrorCode, RpcError};
use crate::types::{
    Answer, ApprovalOutcome, ApprovalPolicy, ConfigEntry, ProviderDescriptor, Question,
    SessionEvent, SessionInfo, ToolDescriptor, TurnSummary,
};

/// Client-to-server method names. String constants, not an enum: an unknown
/// method is answered `MethodNotFound`, never a parse failure.
pub mod method {
    pub const HELLO: &str = "rpc.hello";
    pub const SESSION_CREATE: &str = "session.create";
    pub const SESSION_LIST: &str = "session.list";
    pub const SESSION_EVENTS: &str = "session.events";
    pub const SESSION_FORK: &str = "session.fork";
    pub const SESSION_SUBSCRIBE: &str = "session.subscribe";
    pub const SESSION_UNSUBSCRIBE: &str = "session.unsubscribe";
    pub const AGENT_PROMPT: &str = "agent.prompt";
    pub const AGENT_STATUS: &str = "agent.status";
    pub const AGENT_INTERRUPT: &str = "agent.interrupt";
    pub const AGENT_STEER: &str = "agent.steer";
    pub const CATALOG_TOOLS: &str = "catalog.tools";
    pub const CATALOG_MODELS: &str = "catalog.models";
    pub const CONFIG_DUMP: &str = "config.dump";
    pub const APPROVAL_SET: &str = "approval.set";
}

/// Server-to-client frames. The two notifications are one-way; `UI_ASK` and
/// `UI_APPROVE` are requests the client answers.
pub mod push {
    pub const SESSION_EVENT: &str = "session/event";
    pub const AGENT_STATUS: &str = "agent/status";
    pub const UI_ASK: &str = "ui/ask";
    pub const UI_APPROVE: &str = "ui/approve";
}

/// Capability strings a server advertises in [`HelloResult`]. A surface checks
/// one before it uses an optional call.
pub mod capability {
    pub const SESSION_FORK: &str = "session.fork";
    pub const SESSION_SUBSCRIBE: &str = "session.subscribe";
    pub const AGENT_INTERRUPT: &str = "agent.interrupt";
    pub const AGENT_STEER: &str = "agent.steer";
    pub const UI_ASK: &str = "ui.ask";
    pub const UI_APPROVE: &str = "ui.approve";
    pub const APPROVAL_SET: &str = "approval.set";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloParams {
    /// The `major.minor` the client was built against.
    pub protocol_version: String,
    pub client: PeerInfo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol_version: String,
    pub server: PeerInfo,
    /// Every optional call this build actually serves.
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionCreateParams {
    /// Reuse an existing journal under this id, or omit for a fresh one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Open the journal at this path instead of the server's own directory.
    /// The id is then read from the journal's `session/start` line, which is
    /// how a path becomes an id for every other call. A path with no file yet
    /// is created; a path whose file is not a journal is `LogCorrupt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Provider route. Omit for the server default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Omit for the provider's first catalog entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
}

/// Params for every call that names one session and nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRef {
    pub session_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Empty {}

/// The result of a call whose only answer is "it happened".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ack {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionInfo>,
}

/// One page of a journal. Paging is by `seq`, never by offset, so a page is
/// stable while the log grows underneath it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEventsParams {
    pub session_id: String,
    /// First seq to return, inclusive. `0` reads from the start, and a seq
    /// past the tail answers an empty page with `eof` and `next_seq` at that
    /// tail (contract section 4.4.5).
    #[serde(default)]
    pub from_seq: u64,
    /// Page size, clamped down to the server's own maximum. Omit for that
    /// maximum; `0` reads as absent, because a page of no events would stall a
    /// pager (contract section 4.4.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEventsResult {
    pub events: Vec<SessionEvent>,
    /// `from_seq` for the next page.
    pub next_seq: u64,
    /// True when this page reached the end of the journal.
    pub eof: bool,
}

/// Fork a session: a new journal seeded with a prefix of another one's.
///
/// Contract section 4.4.6. The child is a copy and not a reference, so what is
/// appended to either after the fork never reaches the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionForkParams {
    /// The session to fork from.
    pub session_id: String,
    /// Last parent seq the child inherits, inclusive. Omit for the parent's
    /// last event.
    ///
    /// Deliberately not spelled `from_seq`: that name is the *first* event a
    /// caller receives everywhere else in this contract, and this one is the
    /// last event a child keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through_seq: Option<u64>,
    /// Id for the child. Omit and the server mints one. An id that already has
    /// a journal is refused rather than reopened, which is the one place this
    /// call differs from `session.create`: a seed appended to a journal that
    /// already holds a history would splice two of them together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSubscribeParams {
    pub session_id: String,
    /// Replay from this seq, inclusive, before live delivery starts. Omit to
    /// receive live events only. A seq past the tail replays nothing and is
    /// not an error (contract section 4.4.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSubscribeResult {
    /// Names this subscription for `session.unsubscribe`. One caller may hold
    /// several, and closing one never closes another.
    pub subscription_id: String,
    /// Seq of the last event the subscription starts after; `-1` for an empty
    /// log. Every event with a higher seq arrives as a `session/event` push.
    pub last_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUnsubscribeParams {
    pub subscription_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentPromptParams {
    pub session_id: String,
    pub content: String,
}

/// `agent.prompt` returns when the turn closes. Its events stream meanwhile to
/// every subscriber, so a surface renders progress without polling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentPromptResult {
    pub summary: TurnSummary,
}

/// Params of `agent.steer`: a message for the turn already running.
///
/// Contract section 4.4.10. Not `agent.prompt`: this joins a turn rather than
/// starting one, and is refused when there is none to join.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSteerParams {
    pub session_id: String,
    pub content: String,
}

/// Where a steered message landed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSteerResult {
    /// The turn that took it.
    pub turn: u64,
    /// The step that read it, so a surface can show the message landing where
    /// it landed rather than where it was typed. Absent while it is still
    /// queued at the moment the call answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_at_step: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatusResult {
    #[serde(flatten)]
    pub status: AgentStatusPush,
}

/// Payload of the `agent/status` push, and the body of `agent.status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatusPush {
    pub session_id: String,
    pub state: crate::types::AgentState,
    /// The turn in flight, when one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
}

/// Payload of the `session/event` push.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEventPush {
    pub session_id: String,
    pub event: SessionEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCatalogResult {
    pub tools: Vec<ToolDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalogResult {
    pub providers: Vec<ProviderDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigDumpResult {
    pub entries: Vec<ConfigEntry>,
}

/// Params of the server-to-client `ui/ask` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskParams {
    /// The session the ask belongs to, so a multi-session surface can route it.
    pub session_id: String,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskResult {
    /// One answer per question, in any order, each echoing its question id.
    pub answers: Vec<Answer>,
}

/// Params of the server-to-client `ui/approve` request.
///
/// Contract section 4.4.7. One question about one tool call, put to the client
/// before the call runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApproveParams {
    /// The session the question belongs to, so a multi-session surface can
    /// route it.
    pub session_id: String,
    /// The `id` of the `approval/asked` this question was audited as, so a
    /// surface can pair the prompt it is showing with the journal line.
    pub request_id: String,
    /// The tool the question is about.
    pub tool_name: String,
    /// The `tool/call.id` being decided, when the asker had one. A surface
    /// that already streamed the call attaches the prompt to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// The asker's own words for why it is asking. Text for a human, not a
    /// code to match on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The client's answer to `ui/approve`.
///
/// Every way of not producing one denies: no `ui.approve` capability, a
/// JSON-RPC error, an `outcome` outside the four words, or a connection that
/// dropped. See [`ApprovalOutcome`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApproveResult {
    pub outcome: ApprovalOutcome,
}

/// Params of `approval.set`: write one session's approval policy.
///
/// Contract section 4.4.7. This is the only thing that appends
/// `approval/policy`, and there is no matching getter: a caller folds the
/// events it already receives. Setting the policy a session is already under
/// writes nothing, so a surface may send it idempotently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalSetParams {
    pub session_id: String,
    pub policy: ApprovalPolicy,
}

/// The whole client-to-server surface, as Rust. The JSON-RPC server is a thin
/// codec over this trait, and the CLI calls it in process, so the two cannot
/// serve different contracts.
/// Where a subscription's pushes go.
///
/// This is what makes one contract serve three carriers. The stdio and
/// WebSocket carriers implement it as "serialize and write a frame"; the
/// in-process caller implements it as "hand to the renderer". Neither the
/// engine nor a renderer has to know which it is talking to.
///
/// Delivery is fire and forget: a sink that is gone is dropped by the engine
/// rather than failing the turn that pushed to it.
pub trait EventSink: Send + Sync {
    fn session_event(&self, push: SessionEventPush);
    fn agent_status(&self, push: AgentStatusPush);
}

#[async_trait::async_trait]
pub trait Engine: Send + Sync {
    async fn hello(&self, params: HelloParams) -> Result<HelloResult, RpcError>;
    async fn session_create(&self, params: SessionCreateParams) -> Result<SessionInfo, RpcError>;
    async fn session_list(&self) -> Result<SessionListResult, RpcError>;
    async fn session_events(
        &self,
        params: SessionEventsParams,
    ) -> Result<SessionEventsResult, RpcError>;
    /// Contract section 4.2: reserved.
    ///
    /// The default body is what `Reserved` states in Rust. A build that does
    /// not serve the call answers `NotImplemented` instead of failing to
    /// compile, which is exactly the promise the status makes to a surface
    /// building against a frozen shape. The slice that serves the call deletes
    /// this body, and section 7.4's compile error for every implementor comes
    /// back with it.
    async fn session_fork(&self, params: SessionForkParams) -> Result<SessionInfo, RpcError> {
        let _ = params;
        Err(RpcError::new(
            ErrorCode::NotImplemented,
            format!(
                "`{}` is reserved, and this build does not serve it",
                method::SESSION_FORK
            ),
        )
        .with_data(serde_json::json!({ "method": method::SESSION_FORK })))
    }
    /// The one call whose trait form takes an argument the wire does not
    /// carry: where the carrier wants its pushes delivered.
    async fn session_subscribe(
        &self,
        params: SessionSubscribeParams,
        sink: Arc<dyn EventSink>,
    ) -> Result<SessionSubscribeResult, RpcError>;
    async fn session_unsubscribe(&self, params: SessionUnsubscribeParams) -> Result<Ack, RpcError>;
    async fn agent_prompt(&self, params: AgentPromptParams) -> Result<AgentPromptResult, RpcError>;
    async fn agent_status(&self, params: SessionRef) -> Result<AgentStatusResult, RpcError>;
    async fn agent_interrupt(&self, params: SessionRef) -> Result<Ack, RpcError>;
    /// Contract section 4.2: reserved. See [`Engine::session_fork`] for what a
    /// default body means here.
    async fn agent_steer(&self, params: AgentSteerParams) -> Result<AgentSteerResult, RpcError> {
        let _ = params;
        Err(RpcError::new(
            ErrorCode::NotImplemented,
            format!(
                "`{}` is reserved, and this build does not serve it",
                method::AGENT_STEER
            ),
        )
        .with_data(serde_json::json!({ "method": method::AGENT_STEER })))
    }
    async fn catalog_tools(&self) -> Result<ToolCatalogResult, RpcError>;
    async fn catalog_models(&self) -> Result<ModelCatalogResult, RpcError>;
    async fn config_dump(&self) -> Result<ConfigDumpResult, RpcError>;
    /// Contract section 4.2: reserved. See [`Engine::session_fork`] for what a
    /// default body means here.
    async fn approval_set(&self, params: ApprovalSetParams) -> Result<Ack, RpcError> {
        let _ = params;
        Err(RpcError::new(
            ErrorCode::NotImplemented,
            format!(
                "`{}` is reserved, and this build does not serve it",
                method::APPROVAL_SET
            ),
        )
        .with_data(serde_json::json!({ "method": method::APPROVAL_SET })))
    }
}
