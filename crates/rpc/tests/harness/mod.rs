//! The engine double every carrier conformance suite drives.
//!
//! A carrier moves strings; what the strings mean is the codec's business and
//! what the calls do is the engine's. So a carrier suite needs an engine that
//! answers, records what it was asked, and can hold a turn open on demand -
//! and needs nothing else from a real one. That double is here rather than in
//! one suite, because the contract's promise is that a surface working over
//! one carrier works over the others, and two suites asserting it against two
//! different doubles would not be asserting the same thing.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tetanus_protocol::methods::{
    method, Ack, AgentPromptParams, AgentPromptResult, AgentStatusPush, AgentStatusResult,
    ConfigDumpResult, Engine, EventSink, HelloParams, HelloResult, ModelCatalogResult, PeerInfo,
    SessionCreateParams, SessionEventPush, SessionEventsParams, SessionEventsResult,
    SessionForkParams, SessionListResult, SessionRef, SessionSubscribeParams,
    SessionSubscribeResult, SessionUnsubscribeParams, ToolCatalogResult,
};
use tetanus_protocol::rpc::RpcError;
use tetanus_protocol::types::{AgentState, SessionEvent, SessionInfo, StopReason, TurnSummary};
use tetanus_protocol::PROTOCOL_VERSION;
use tokio::sync::Notify;

/// An engine that records what it was asked and holds a turn open on demand.
#[derive(Default)]
pub struct Fake {
    calls: Mutex<Vec<String>>,
    /// The sink `session.subscribe` was given, kept so a case can push through
    /// it the way a running turn would.
    sink: Mutex<Option<Arc<dyn EventSink>>>,
    /// `agent.prompt` waits on this; `agent.interrupt` releases it.
    turn: Notify,
}

impl Fake {
    fn record(&self, name: &str) {
        self.calls.lock().expect("calls").push(name.to_string());
    }

    pub fn called(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }

    pub fn sink(&self) -> Arc<dyn EventSink> {
        self.sink.lock().expect("sink").clone().expect("a sink")
    }

    pub fn push(&self, event: SessionEvent) {
        self.sink().session_event(SessionEventPush {
            session_id: "s1".into(),
            event,
        });
    }

    pub fn push_status(&self, state: AgentState) {
        self.sink().agent_status(AgentStatusPush {
            session_id: "s1".into(),
            state,
            turn: Some(1),
            step: None,
        });
    }
}

fn unused<T>() -> T {
    unreachable!("no case in this file makes this call")
}

#[async_trait::async_trait]
impl Engine for Fake {
    async fn hello(&self, _: HelloParams) -> Result<HelloResult, RpcError> {
        self.record(method::HELLO);
        Ok(HelloResult {
            protocol_version: PROTOCOL_VERSION.into(),
            server: PeerInfo {
                name: "tetanus".into(),
                version: "0".into(),
            },
            capabilities: vec![method::SESSION_SUBSCRIBE.into()],
        })
    }
    async fn session_subscribe(
        &self,
        _: SessionSubscribeParams,
        sink: Arc<dyn EventSink>,
    ) -> Result<SessionSubscribeResult, RpcError> {
        self.record(method::SESSION_SUBSCRIBE);
        *self.sink.lock().expect("sink") = Some(sink);
        Ok(SessionSubscribeResult {
            subscription_id: "sub-1".into(),
            last_seq: 0,
        })
    }
    async fn session_unsubscribe(&self, params: SessionUnsubscribeParams) -> Result<Ack, RpcError> {
        self.record(&format!(
            "{} {}",
            method::SESSION_UNSUBSCRIBE,
            params.subscription_id
        ));
        *self.sink.lock().expect("sink") = None;
        Ok(Ack { ok: true })
    }
    async fn agent_prompt(&self, _: AgentPromptParams) -> Result<AgentPromptResult, RpcError> {
        self.record(method::AGENT_PROMPT);
        self.turn.notified().await;
        Ok(AgentPromptResult {
            summary: TurnSummary {
                turn: 1,
                steps: 1,
                stop_reason: StopReason::Natural,
                stop_veto: None,
                content: "done".into(),
                duration_ms: None,
                usage: None,
            },
        })
    }
    async fn agent_interrupt(&self, _: SessionRef) -> Result<Ack, RpcError> {
        self.record(method::AGENT_INTERRUPT);
        self.turn.notify_one();
        Ok(Ack { ok: true })
    }
    async fn session_create(&self, _: SessionCreateParams) -> Result<SessionInfo, RpcError> {
        unused()
    }
    async fn session_fork(&self, _: SessionForkParams) -> Result<SessionInfo, RpcError> {
        unused()
    }
    async fn session_events(
        &self,
        _: SessionEventsParams,
    ) -> Result<SessionEventsResult, RpcError> {
        unused()
    }
    async fn agent_status(&self, _: SessionRef) -> Result<AgentStatusResult, RpcError> {
        unused()
    }
    async fn session_list(&self) -> Result<SessionListResult, RpcError> {
        unused()
    }
    async fn catalog_tools(&self) -> Result<ToolCatalogResult, RpcError> {
        unused()
    }
    async fn catalog_models(&self) -> Result<ModelCatalogResult, RpcError> {
        unused()
    }
    async fn config_dump(&self) -> Result<ConfigDumpResult, RpcError> {
        unused()
    }
}

/// The handshake frame every connection opens with. Contract section 4.4.1.
pub fn hello() -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": method::HELLO,
        "params": {
            "protocol_version": PROTOCOL_VERSION,
            "client": { "name": "t", "version": "0" },
        },
    })
}
