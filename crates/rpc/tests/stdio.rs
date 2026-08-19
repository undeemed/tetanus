//! Conformance for the stdio carrier.
//!
//! Test design: the carrier moves strings, so every case drives it through a
//! real pipe and reads back real lines. The engine behind it is a double whose
//! `agent.prompt` blocks until something else releases it, which is what lets a
//! case pin concurrency rather than assume it.
//!
//! Environmental needs: none. The pipe is `tokio::io::duplex`, in memory, so no
//! case opens a file, a socket or a session.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tetanus_protocol::methods::{
    method, push, Ack, AgentPromptParams, AgentPromptResult, AgentStatusPush, AgentStatusResult,
    ConfigDumpResult, Engine, EventSink, HelloParams, HelloResult, ModelCatalogResult, PeerInfo,
    SessionCreateParams, SessionEventPush, SessionEventsParams, SessionEventsResult,
    SessionListResult, SessionRef, SessionSubscribeParams, SessionSubscribeResult,
    SessionUnsubscribeParams, ToolCatalogResult,
};
use tetanus_protocol::rpc::RpcError;
use tetanus_protocol::types::{AgentState, SessionEvent, SessionInfo, StopReason, TurnSummary};
use tetanus_protocol::PROTOCOL_VERSION;
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// An engine that records what it was asked and holds a turn open on demand.
#[derive(Default)]
struct Fake {
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

    fn called(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }

    fn sink(&self) -> Arc<dyn EventSink> {
        self.sink.lock().expect("sink").clone().expect("a sink")
    }

    fn push(&self, event: SessionEvent) {
        self.sink().session_event(SessionEventPush {
            session_id: "s1".into(),
            event,
        });
    }

    fn push_status(&self, state: AgentState) {
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

/// The peer's end of one connection.
struct Peer {
    write: WriteHalf<DuplexStream>,
    read: Lines<BufReader<ReadHalf<DuplexStream>>>,
    served: JoinHandle<std::io::Result<()>>,
}

impl Peer {
    async fn send(&mut self, frame: serde_json::Value) {
        self.raw(&format!("{frame}\n")).await;
    }

    async fn raw(&mut self, bytes: &str) {
        self.write
            .write_all(bytes.as_bytes())
            .await
            .expect("the peer writes");
    }

    async fn line(&mut self) -> serde_json::Value {
        let line = self
            .read
            .next_line()
            .await
            .expect("the carrier writes")
            .expect("a line");
        serde_json::from_str(&line).expect("a frame is JSON")
    }

    /// Hang up, and wait for the carrier to finish.
    ///
    /// The write half is shut down rather than dropped: the two halves of a
    /// split stream share it, so dropping one closes nothing and the carrier
    /// would wait for a peer that has gone.
    async fn hangup(mut self) {
        self.write.shutdown().await.expect("the peer hangs up");
        self.served
            .await
            .expect("the carrier does not panic")
            .expect("the carrier ends cleanly");
    }
}

fn connect(engine: Arc<Fake>) -> Peer {
    let (peer, carrier) = tokio::io::duplex(4096);
    let (input, output) = tokio::io::split(carrier);
    let served = tokio::spawn(tetanus_rpc::stdio::serve(engine, input, output));
    let (read, write) = tokio::io::split(peer);
    Peer {
        write,
        read: BufReader::new(read).lines(),
        served,
    }
}

fn hello() -> serde_json::Value {
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

/// A connected peer whose handshake has already happened.
async fn greeted(engine: Arc<Fake>) -> Peer {
    let mut peer = connect(engine);
    peer.send(hello()).await;
    peer.line().await;
    peer
}

/// TC-STDIO-1: contract section 4.1. A request is answered with one object on
/// one line, correlated to the request's id.
#[tokio::test]
async fn a_request_is_answered_on_one_line() {
    let mut peer = connect(Arc::new(Fake::default()));

    peer.send(hello()).await;

    let answer = peer.line().await;
    assert_eq!(answer["jsonrpc"], "2.0");
    assert_eq!(answer["id"], 0);
    assert_eq!(answer["result"]["protocol_version"], PROTOCOL_VERSION);
    peer.hangup().await;
}

/// TC-STDIO-2: a line that asks no question is answered with no line. A
/// notification is one-way, and a blank line carries no frame at all. Both are
/// proved by the next request's answer being the next line to arrive.
#[tokio::test]
async fn a_line_that_asks_nothing_is_answered_with_no_line() {
    let mut peer = connect(Arc::new(Fake::default()));

    peer.send(json!({ "jsonrpc": "2.0", "method": "session/event" }))
        .await;
    peer.raw("\n   \n").await;
    peer.send(hello()).await;

    assert_eq!(
        peer.line().await["id"],
        0,
        "the first line back is the request's answer"
    );
    peer.hangup().await;
}

/// TC-STDIO-3: contract section 4.1. Either push reaches the peer as a
/// JSON-RPC notification frame, params verbatim, on its own line.
#[tokio::test]
async fn a_push_arrives_as_a_notification_frame() {
    let engine = Arc::new(Fake::default());
    let mut peer = greeted(engine.clone()).await;

    peer.send(
        json!({ "jsonrpc": "2.0", "id": 1, "method": method::SESSION_SUBSCRIBE,
                      "params": { "session_id": "s1" } }),
    )
    .await;
    assert_eq!(peer.line().await["result"]["subscription_id"], "sub-1");

    engine.push(SessionEvent {
        ty: "assistant/message".into(),
        seq: 4,
        time: 1,
        data: json!({ "content": "hi" }),
        source_event_seqs: None,
    });

    let frame = peer.line().await;
    assert_eq!(frame["method"], push::SESSION_EVENT);
    assert!(frame.get("id").is_none(), "a notification has no id");
    assert_eq!(frame["params"]["session_id"], "s1");
    assert_eq!(frame["params"]["event"]["seq"], 4);
    assert_eq!(frame["params"]["event"]["data"]["content"], "hi");

    engine.push_status(AgentState::Running);

    let frame = peer.line().await;
    assert_eq!(frame["method"], push::AGENT_STATUS);
    assert_eq!(frame["params"]["state"], "running");
    assert_eq!(frame["params"]["turn"], 1);
    assert!(
        frame["params"].get("step").is_none(),
        "an absent field is absent, not null"
    );
    peer.hangup().await;
}

/// TC-STDIO-4: a peer that hangs up leaves nothing open. The connection's
/// subscriptions are closed with the ids it was given, so the engine stops
/// pushing into a socket nobody reads.
#[tokio::test]
async fn hanging_up_closes_the_connection_subscriptions() {
    let engine = Arc::new(Fake::default());
    let mut peer = greeted(engine.clone()).await;

    peer.send(
        json!({ "jsonrpc": "2.0", "id": 1, "method": method::SESSION_SUBSCRIBE,
                      "params": { "session_id": "s1" } }),
    )
    .await;
    peer.line().await;
    peer.hangup().await;

    assert_eq!(
        engine.called().last().map(String::as_str),
        Some("session.unsubscribe sub-1"),
        "the id the carrier handed out is the id it closed"
    );
}

/// TC-STDIO-5: contract section 4.4.2. A call is read and answered while an
/// earlier call is still running, which is what makes `agent.interrupt`
/// reachable during the turn it interrupts.
#[tokio::test]
async fn a_call_is_answered_while_an_earlier_one_runs() {
    let engine = Arc::new(Fake::default());
    let mut peer = greeted(engine.clone()).await;

    peer.send(
        json!({ "jsonrpc": "2.0", "id": 1, "method": method::AGENT_PROMPT,
                      "params": { "session_id": "s1", "content": "hi" } }),
    )
    .await;
    peer.send(
        json!({ "jsonrpc": "2.0", "id": 2, "method": method::AGENT_INTERRUPT,
                      "params": { "session_id": "s1" } }),
    )
    .await;

    let first = peer.line().await;
    assert_eq!(
        first["id"], 2,
        "the interrupt is answered while the turn is still running"
    );
    assert_eq!(first["result"]["ok"], true);

    let second = peer.line().await;
    assert_eq!(second["id"], 1, "the turn's own answer follows");
    assert_eq!(second["result"]["summary"]["stop_reason"], "natural");
    peer.hangup().await;
}
