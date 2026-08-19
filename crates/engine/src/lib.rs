//! The engine behind `docs/interface-contract.md`.
//!
//! [`HarnessEngine`] is the one implementation of [`tetanus_protocol::Engine`].
//! The JSON-RPC carriers and the CLI both drive it, so no surface can serve a
//! different contract from another, and adding a call is a compile error in
//! every surface that has not handled it.
//!
//! This crate is a library. It prints nothing, and it owns no binary: the
//! presentation lane owns the binary and wires each subcommand to the calls
//! section 4.7 of the contract lists for it.
//!
//! A call this build does not serve yet answers `NotImplemented` naming
//! itself, which is the contract's way of letting a surface tell "not yet"
//! from "went wrong".

pub mod convert;

use std::sync::Arc;

use tetanus_protocol::methods::{
    method, Ack, AgentPromptParams, AgentPromptResult, AgentStatusResult, ConfigDumpResult, Engine,
    EventSink, HelloParams, HelloResult, ModelCatalogResult, PeerInfo, SessionCreateParams,
    SessionEventsParams, SessionEventsResult, SessionListResult, SessionRef,
    SessionSubscribeParams, SessionSubscribeResult, SessionUnsubscribeParams, ToolCatalogResult,
};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types::SessionInfo;
use tetanus_protocol::{is_compatible, PROTOCOL_VERSION};

use crate::convert::not_implemented;

#[derive(Default)]
pub struct HarnessEngine {
    _private: (),
}

impl HarnessEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// The optional calls this build actually serves. A surface hides an
    /// affordance whose capability is absent, rather than discovering the
    /// absence as an error.
    pub fn capabilities(&self) -> Vec<String> {
        Vec::new()
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

    async fn session_create(&self, _: SessionCreateParams) -> Result<SessionInfo, RpcError> {
        Err(not_implemented(method::SESSION_CREATE))
    }

    async fn session_list(&self) -> Result<SessionListResult, RpcError> {
        Err(not_implemented(method::SESSION_LIST))
    }

    async fn session_events(
        &self,
        _: SessionEventsParams,
    ) -> Result<SessionEventsResult, RpcError> {
        Err(not_implemented(method::SESSION_EVENTS))
    }

    async fn session_subscribe(
        &self,
        _: SessionSubscribeParams,
        _: Arc<dyn EventSink>,
    ) -> Result<SessionSubscribeResult, RpcError> {
        Err(not_implemented(method::SESSION_SUBSCRIBE))
    }

    async fn session_unsubscribe(&self, _: SessionUnsubscribeParams) -> Result<Ack, RpcError> {
        Err(not_implemented(method::SESSION_UNSUBSCRIBE))
    }

    async fn agent_prompt(&self, _: AgentPromptParams) -> Result<AgentPromptResult, RpcError> {
        Err(not_implemented(method::AGENT_PROMPT))
    }

    async fn agent_status(&self, _: SessionRef) -> Result<AgentStatusResult, RpcError> {
        Err(not_implemented(method::AGENT_STATUS))
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
