//! Every call the contract defines: its name, its params, its result, and the
//! facade both surfaces drive. One method per call, so the RPC server and the
//! in-process CLI client cannot diverge.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::rpc::RpcError;
use crate::types::{
    Answer, ConfigEntry, ProviderDescriptor, Question, SessionEvent, SessionInfo, ToolDescriptor,
    TurnSummary,
};

/// Client-to-server method names. String constants, not an enum: an unknown
/// method is answered `MethodNotFound`, never a parse failure.
pub mod method {
    pub const HELLO: &str = "rpc.hello";
    pub const SESSION_CREATE: &str = "session.create";
    pub const SESSION_LIST: &str = "session.list";
    pub const SESSION_EVENTS: &str = "session.events";
    pub const SESSION_SUBSCRIBE: &str = "session.subscribe";
    pub const SESSION_UNSUBSCRIBE: &str = "session.unsubscribe";
    pub const AGENT_PROMPT: &str = "agent.prompt";
    pub const AGENT_STATUS: &str = "agent.status";
    pub const AGENT_INTERRUPT: &str = "agent.interrupt";
    pub const CATALOG_TOOLS: &str = "catalog.tools";
    pub const CATALOG_MODELS: &str = "catalog.models";
    pub const CONFIG_DUMP: &str = "config.dump";
}

/// Server-to-client frames. The two notifications are one-way; `UI_ASK` is a
/// request the client answers.
pub mod push {
    pub const SESSION_EVENT: &str = "session/event";
    pub const AGENT_STATUS: &str = "agent/status";
    pub const UI_ASK: &str = "ui/ask";
}

/// Capability strings a server advertises in [`HelloResult`]. A surface checks
/// one before it uses an optional call.
pub mod capability {
    pub const SESSION_SUBSCRIBE: &str = "session.subscribe";
    pub const AGENT_INTERRUPT: &str = "agent.interrupt";
    pub const UI_ASK: &str = "ui.ask";
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
    /// First seq to return. `0` reads from the start.
    #[serde(default)]
    pub from_seq: u64,
    /// Server clamps to its own maximum; omit for that maximum.
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSubscribeParams {
    pub session_id: String,
    /// Replay from this seq before live delivery starts. Omit to receive live
    /// events only.
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
    async fn catalog_tools(&self) -> Result<ToolCatalogResult, RpcError>;
    async fn catalog_models(&self) -> Result<ModelCatalogResult, RpcError>;
    async fn config_dump(&self) -> Result<ConfigDumpResult, RpcError>;
}
