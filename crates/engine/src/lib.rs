//! The engine behind `docs/interface-contract.md`.
//!
//! [`HarnessEngine`] is the one implementation of [`tetanus_protocol::Engine`].
//! The JSON-RPC carriers and the CLI both drive it, so no surface can serve a
//! different contract from another.
//!
//! This crate is a library. It prints nothing, and it owns no binary: the
//! presentation lane owns the binary and wires each subcommand to the calls
//! section 4.7 of the contract lists for it.

pub mod agent;
pub mod convert;
pub mod session;
pub mod subscribe;

use std::path::PathBuf;
use std::sync::Arc;

use tetanus_protocol::methods::{
    capability, method, Ack, AgentPromptParams, AgentPromptResult, AgentStatusResult,
    ConfigDumpResult, Engine, EventSink, HelloParams, HelloResult, ModelCatalogResult, PeerInfo,
    SessionCreateParams, SessionEventsParams, SessionEventsResult, SessionListResult, SessionRef,
    SessionSubscribeParams, SessionSubscribeResult, SessionUnsubscribeParams, ToolCatalogResult,
};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types::SessionInfo;
use tetanus_protocol::{is_compatible, PROTOCOL_VERSION};
use tetanus_turn::tools::{EchoTool, ToolRegistry};

use crate::agent::{MockProviders, Providers, Runtime};
use crate::convert::not_implemented;
use crate::session::{SessionDefaults, SessionStore};
use crate::subscribe::Hub;

/// Everything the engine needs that is not a call.
#[derive(Clone)]
pub struct EngineConfig {
    /// Directory holding one JSONL journal per session.
    pub sessions_root: PathBuf,
    /// Provider a `session.create` with no override resolves to.
    pub default_provider: String,
    /// Model a `session.create` with no override resolves to.
    pub default_model: String,
    pub max_steps: u32,
    /// The adapter behind each provider a session may name.
    pub providers: Arc<dyn Providers>,
    /// The tools every turn on this engine can call.
    pub tools: Arc<ToolRegistry>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            sessions_root: PathBuf::from("sessions"),
            default_provider: tetanus_turn::llm::mock::PROVIDER.to_string(),
            default_model: tetanus_turn::llm::mock::MODEL.to_string(),
            max_steps: 8,
            // Offline by default: a build with no configuration still runs a
            // full documented turn, with no key and no network.
            providers: Arc::new(MockProviders),
            tools: Arc::new(ToolRegistry::new().with(Arc::new(EchoTool))),
        }
    }
}

pub struct HarnessEngine {
    sessions: Arc<SessionStore>,
    hub: Arc<Hub>,
    runtime: Arc<Runtime>,
}

impl HarnessEngine {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            sessions: Arc::new(SessionStore::new(
                config.sessions_root.clone(),
                SessionDefaults {
                    provider: config.default_provider.clone(),
                    model: config.default_model.clone(),
                    max_steps: config.max_steps,
                },
            )),
            hub: Arc::new(Hub::new()),
            runtime: Arc::new(Runtime::new(config.providers, config.tools)),
        }
    }

    pub fn sessions(&self) -> &Arc<SessionStore> {
        &self.sessions
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// The optional calls this build actually serves. A surface hides an
    /// affordance whose capability is absent, rather than discovering the
    /// absence as an error.
    pub fn capabilities(&self) -> Vec<String> {
        // A capability is a promise that the call behind it is served.
        vec![capability::SESSION_SUBSCRIBE.to_string()]
    }
}

#[async_trait::async_trait]
impl Engine for HarnessEngine {
    async fn hello(&self, params: HelloParams) -> Result<HelloResult, RpcError> {
        if !is_compatible(&params.protocol_version) {
            return Err(RpcError::new(
                ErrorCode::UnsupportedProtocolVersion,
                format!(
                    "this build serves contract {PROTOCOL_VERSION}, the client asked for {}",
                    params.protocol_version
                ),
            )
            .with_data(serde_json::json!({
                "server": PROTOCOL_VERSION,
                "client": params.protocol_version,
            })));
        }
        Ok(HelloResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            server: PeerInfo {
                name: "tetanus".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: self.capabilities(),
        })
    }

    async fn session_create(&self, params: SessionCreateParams) -> Result<SessionInfo, RpcError> {
        self.sessions.create(params)
    }

    async fn session_list(&self) -> Result<SessionListResult, RpcError> {
        Ok(SessionListResult {
            sessions: self.sessions.list()?,
        })
    }

    async fn session_events(
        &self,
        params: SessionEventsParams,
    ) -> Result<SessionEventsResult, RpcError> {
        self.sessions
            .events(&params.session_id, params.from_seq, params.limit)
    }

    async fn session_subscribe(
        &self,
        params: SessionSubscribeParams,
        sink: Arc<dyn EventSink>,
    ) -> Result<SessionSubscribeResult, RpcError> {
        let session = self.sessions.open(&params.session_id)?;
        Ok(self.hub.subscribe(&session, params.from_seq, sink))
    }

    async fn session_unsubscribe(&self, params: SessionUnsubscribeParams) -> Result<Ack, RpcError> {
        self.hub.unsubscribe(params)
    }

    async fn agent_prompt(&self, params: AgentPromptParams) -> Result<AgentPromptResult, RpcError> {
        self.runtime.prompt(&self.sessions, &self.hub, params).await
    }

    async fn agent_status(&self, params: SessionRef) -> Result<AgentStatusResult, RpcError> {
        self.runtime.status(&self.sessions, &params.session_id)
    }

    async fn agent_interrupt(&self, _: SessionRef) -> Result<Ack, RpcError> {
        Err(not_implemented(method::AGENT_INTERRUPT))
    }

    async fn catalog_tools(&self) -> Result<ToolCatalogResult, RpcError> {
        Err(not_implemented(method::CATALOG_TOOLS))
    }

    async fn catalog_models(&self) -> Result<ModelCatalogResult, RpcError> {
        Err(not_implemented(method::CATALOG_MODELS))
    }

    async fn config_dump(&self) -> Result<ConfigDumpResult, RpcError> {
        Err(not_implemented(method::CONFIG_DUMP))
    }
}
