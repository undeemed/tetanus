//! Conformance for the WebSocket carrier.
//!
//! Test design: contract section 4.1 says every carrier moves the same
//! payloads, so this suite restates the stdio suite's claims (TC-STDIO-1..5)
//! against a real socket. What only this carrier can be asked - framing that
//! is not text, and two peers on one server - follows in TC-WS-6 and TC-WS-7.
//!
//! Behind it is `harness::Fake`, the same double the stdio suite drives, so a
//! difference between the two suites is a difference between the carriers.
//!
//! Environmental needs: a loopback TCP port. Every case binds `127.0.0.1:0`,
//! so no case needs a fixed port and cases may run in parallel. No case opens
//! a file or a session, and none reaches the network.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tetanus_protocol::methods::{method, push};
use tetanus_protocol::types::{AgentState, SessionEvent};
use tetanus_protocol::PROTOCOL_VERSION;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

mod harness;
use harness::{hello, Fake};

/// A server bound to a loopback port, with a peer connected to it.
struct Peer {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Peer {
    async fn send(&mut self, frame: serde_json::Value) {
        self.socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .expect("the peer writes");
    }

    /// The next frame the carrier wrote, as JSON.
    async fn frame(&mut self) -> serde_json::Value {
        loop {
            match self.socket.next().await.expect("the carrier writes") {
                Ok(Message::Text(text)) => {
                    break serde_json::from_str(text.as_str()).expect("a frame is JSON")
                }
                Ok(Message::Ping(_) | Message::Pong(_)) => continue,
                other => panic!("expected a text frame, got {other:?}"),
            }
        }
    }

    /// Hang up, and give the carrier a moment to notice.
    async fn hangup(mut self) {
        self.socket.close(None).await.expect("the peer hangs up");
        while self
            .socket
            .next()
            .await
            .transpose()
            .unwrap_or(None)
            .is_some()
        {}
    }
}

/// Start a server on a loopback port and return its `ws://` address.
async fn host(engine: Arc<Fake>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let address = listener.local_addr().expect("the bound address");
    tokio::spawn(tetanus_rpc::websocket::serve(engine, listener));
    format!("ws://{address}")
}

async fn connect(address: &str) -> Peer {
    let (socket, _) = tokio_tungstenite::connect_async(address)
        .await
        .expect("the handshake succeeds");
    Peer { socket }
}

/// A connected peer whose handshake has already happened.
async fn greeted(address: &str) -> Peer {
    let mut peer = connect(address).await;
    peer.send(hello()).await;
    peer.frame().await;
    peer
}

/// TC-WS-1: contract section 4.1. A request is answered with one JSON object in
/// one text frame, correlated to the request's id.
#[tokio::test]
async fn a_request_is_answered_in_one_text_frame() {
    let address = host(Arc::new(Fake::default())).await;
    let mut peer = connect(&address).await;

    peer.send(hello()).await;

    let answer = peer.frame().await;
    assert_eq!(answer["jsonrpc"], "2.0");
    assert_eq!(answer["id"], 0);
    assert_eq!(answer["result"]["protocol_version"], PROTOCOL_VERSION);
    peer.hangup().await;
}

/// TC-WS-2: a frame that asks no question is answered with no frame. A
/// notification is one-way, proved by the next request's answer being the next
/// frame to arrive.
#[tokio::test]
async fn a_frame_that_asks_nothing_is_answered_with_no_frame() {
    let address = host(Arc::new(Fake::default())).await;
    let mut peer = connect(&address).await;

    peer.send(json!({ "jsonrpc": "2.0", "method": "session/event" }))
        .await;
    peer.send(hello()).await;

    assert_eq!(
        peer.frame().await["id"],
        0,
        "the first frame back is the request's answer"
    );
    peer.hangup().await;
}

/// TC-WS-3: contract section 4.1. Either push reaches the peer as a JSON-RPC
/// notification frame, params verbatim, in its own text frame.
#[tokio::test]
async fn a_push_arrives_as_a_notification_frame() {
    let engine = Arc::new(Fake::default());
    let address = host(engine.clone()).await;
    let mut peer = greeted(&address).await;

    peer.send(
        json!({ "jsonrpc": "2.0", "id": 1, "method": method::SESSION_SUBSCRIBE,
                      "params": { "session_id": "s1" } }),
    )
    .await;
    assert_eq!(peer.frame().await["result"]["subscription_id"], "sub-1");

    engine.push(SessionEvent {
        ty: "assistant/message".into(),
        seq: 4,
        time: 1,
        data: json!({ "content": "hi" }),
        source_event_seqs: None,
    });

    let frame = peer.frame().await;
    assert_eq!(frame["method"], push::SESSION_EVENT);
    assert!(frame.get("id").is_none(), "a notification has no id");
    assert_eq!(frame["params"]["session_id"], "s1");
    assert_eq!(frame["params"]["event"]["seq"], 4);
    assert_eq!(frame["params"]["event"]["data"]["content"], "hi");

    engine.push_status(AgentState::Running);

    let frame = peer.frame().await;
    assert_eq!(frame["method"], push::AGENT_STATUS);
    assert_eq!(frame["params"]["state"], "running");
    assert_eq!(frame["params"]["turn"], 1);
    assert!(
        frame["params"].get("step").is_none(),
        "an absent field is absent, not null"
    );
    peer.hangup().await;
}

/// TC-WS-4: a peer that hangs up leaves nothing open. The connection's
/// subscriptions are closed with the ids it was given, so the engine stops
/// pushing into a socket nobody reads.
#[tokio::test]
async fn hanging_up_closes_the_connection_subscriptions() {
    let engine = Arc::new(Fake::default());
    let address = host(engine.clone()).await;
    let mut peer = greeted(&address).await;

    peer.send(
        json!({ "jsonrpc": "2.0", "id": 1, "method": method::SESSION_SUBSCRIBE,
                      "params": { "session_id": "s1" } }),
    )
    .await;
    peer.frame().await;
    peer.hangup().await;

    settled(&engine, "session.unsubscribe sub-1").await;
}

/// TC-WS-5: contract section 4.4.2. A call is read and answered while an
/// earlier call is still running, which is what makes `agent.interrupt`
/// reachable during the turn it interrupts.
#[tokio::test]
async fn a_call_is_answered_while_an_earlier_one_runs() {
    let engine = Arc::new(Fake::default());
    let address = host(engine.clone()).await;
    let mut peer = greeted(&address).await;

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

    let first = peer.frame().await;
    assert_eq!(
        first["id"], 2,
        "the interrupt is answered while the turn is still running"
    );
    assert_eq!(first["result"]["ok"], true);

    let second = peer.frame().await;
    assert_eq!(second["id"], 1, "the turn's own answer follows");
    assert_eq!(second["result"]["summary"]["stop_reason"], "natural");
    peer.hangup().await;
}

/// Wait for the engine to have been asked `call`.
///
/// A hangup is one-sided: the peer's close frame is on the wire before the
/// carrier has read it, so a case that asserts on what the carrier did next
/// has to wait for it rather than assume it.
async fn settled(engine: &Fake, call: &str) {
    for _ in 0..200 {
        if engine.called().iter().any(|made| made == call) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!(
        "`{call}` was never made; the engine saw {:?}",
        engine.called()
    );
}
