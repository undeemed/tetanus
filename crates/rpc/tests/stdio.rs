//! Conformance for the stdio carrier.
//!
//! Test design: the carrier moves strings, so every case drives it through a
//! real pipe and reads back real lines. Behind it is `harness::Fake`, the
//! double every carrier suite shares, whose `agent.prompt` blocks until
//! something else releases it - which is what lets a case pin concurrency
//! rather than assume it.
//!
//! Environmental needs: none. The pipe is `tokio::io::duplex`, in memory, so no
//! case opens a file, a socket or a session.

use std::sync::Arc;

use serde_json::json;
use tetanus_protocol::methods::{method, push};
use tetanus_protocol::types::{AgentState, SessionEvent};
use tetanus_protocol::PROTOCOL_VERSION;
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tokio::task::JoinHandle;

mod harness;
use harness::{hello, Fake};

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
