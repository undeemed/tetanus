//! Conformance for the push hub: `session.subscribe` and
//! `session.unsubscribe`, and the `EventSink` seam that makes one renderer
//! serve every carrier.
//!
//! Test design: the sink here is a recording one, which is what an in-process
//! caller supplies. An RPC carrier supplies a serializing one. Neither the
//! engine nor these cases know the difference, which is the point.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    capability, AgentStatusPush, Engine, EventSink, HelloParams, PeerInfo, SessionCreateParams,
    SessionEventPush, SessionRef, SessionSubscribeParams, SessionUnsubscribeParams,
};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::PROTOCOL_VERSION;
use tetanus_session::SessionLog;

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<SessionEventPush>>,
}

impl Recorder {
    fn seqs(&self) -> Vec<u64> {
        self.events
            .lock()
            .expect("events")
            .iter()
            .map(|push| push.event.seq)
            .collect()
    }
}

impl EventSink for Recorder {
    fn session_event(&self, push: SessionEventPush) {
        self.events.lock().expect("events").push(push);
    }
    fn agent_status(&self, _: AgentStatusPush) {}
}

fn engine() -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });
    (engine, dir)
}

async fn session(engine: &HarnessEngine) -> String {
    engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create")
        .session_id
}

fn append(engine: &HarnessEngine, session_id: &str, n: u64) {
    let live = engine.sessions().live(session_id).expect("live");
    live.log
        .append("turn/start", serde_json::json!({ "turn": n }))
        .expect("append");
}

/// TC-SUB-1: a live subscription pushes every append, and `last_seq` is the
/// boundary the caller was told about, so no event is delivered twice and none
/// is missed.
#[tokio::test]
async fn live_events_arrive_after_last_seq() {
    let (engine, _dir) = engine();
    let id = session(&engine).await;
    let sink = Arc::new(Recorder::default());

    let result = engine
        .session_subscribe(
            SessionSubscribeParams {
                session_id: id.clone(),
                from_seq: None,
            },
            sink.clone(),
        )
        .await
        .expect("subscribe");
    assert_eq!(result.last_seq, 0, "the header is already on the journal");
    assert!(
        sink.seqs().is_empty(),
        "a live subscription replays nothing"
    );

    append(&engine, &id, 1);
    append(&engine, &id, 2);
    assert_eq!(sink.seqs(), vec![1, 2]);
}

/// TC-SUB-2: `from_seq` replays the journal before live delivery starts, and
/// the join is seamless: each seq arrives exactly once, in order.
#[tokio::test]
async fn a_replay_joins_the_live_stream_without_a_gap_or_a_repeat() {
    let (engine, _dir) = engine();
    let id = session(&engine).await;
    for n in 1..=3 {
        append(&engine, &id, n);
    }

    let sink = Arc::new(Recorder::default());
    let result = engine
        .session_subscribe(
            SessionSubscribeParams {
                session_id: id.clone(),
                from_seq: Some(0),
            },
            sink.clone(),
        )
        .await
        .expect("subscribe");
    assert_eq!(result.last_seq, 3);
    assert_eq!(sink.seqs(), vec![0, 1, 2, 3], "the whole journal replays");

    append(&engine, &id, 4);
    assert_eq!(sink.seqs(), vec![0, 1, 2, 3, 4]);
}

/// TC-SUB-3: two subscriptions on one session are independent. Closing one
/// leaves the other delivering, which is why unsubscribe names a subscription
/// and not a session.
#[tokio::test]
async fn closing_one_subscription_leaves_the_other_running() {
    let (engine, _dir) = engine();
    let id = session(&engine).await;
    let first = Arc::new(Recorder::default());
    let second = Arc::new(Recorder::default());

    let params = SessionSubscribeParams {
        session_id: id.clone(),
        from_seq: None,
    };
    let a = engine
        .session_subscribe(params.clone(), first.clone())
        .await
        .expect("subscribe");
    let b = engine
        .session_subscribe(params, second.clone())
        .await
        .expect("subscribe");
    assert_ne!(a.subscription_id, b.subscription_id);

    append(&engine, &id, 1);
    let closed = engine
        .session_unsubscribe(SessionUnsubscribeParams {
            subscription_id: a.subscription_id.clone(),
        })
        .await
        .expect("unsubscribe");
    assert!(closed.ok);

    append(&engine, &id, 2);
    assert_eq!(first.seqs(), vec![1], "a closed subscription stops");
    assert_eq!(second.seqs(), vec![1, 2], "the other keeps going");

    // Closing the same id twice is a race, not a fault.
    let again = engine
        .session_unsubscribe(SessionUnsubscribeParams {
            subscription_id: a.subscription_id,
        })
        .await
        .expect("a second close is not an error");
    assert!(!again.ok);
}

/// TC-SUB-4: a subscriber names a session it did not create. A cold journal is
/// opened for it; a session that is nowhere is `SessionNotFound`.
#[tokio::test]
async fn subscribing_opens_a_cold_journal() {
    let dir = TempDir::new().expect("temp dir");
    let config = EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    };
    let cold = HarnessEngine::new(config.clone());
    let id = session(&cold).await;
    drop(cold);

    let engine = HarnessEngine::new(config);
    let sink = Arc::new(Recorder::default());
    let result = engine
        .session_subscribe(
            SessionSubscribeParams {
                session_id: id,
                from_seq: Some(0),
            },
            sink.clone(),
        )
        .await
        .expect("subscribe to a journal this process never created");
    assert_eq!(result.last_seq, 0);
    assert_eq!(sink.seqs(), vec![0]);

    let missing = engine
        .session_subscribe(
            SessionSubscribeParams {
                session_id: "nowhere".into(),
                from_seq: None,
            },
            sink,
        )
        .await
        .expect_err("no such session");
    assert_eq!(missing.kind(), Some(ErrorCode::SessionNotFound));
}

/// TC-SUB-5: a capability string is a promise that the call behind it is
/// served. This asserts the promise in both directions, and supersedes
/// TC-SESS-5, which could only assert the empty case.
#[tokio::test]
async fn every_advertised_capability_is_served_and_no_other() {
    let (engine, _dir) = engine();
    let id = session(&engine).await;
    let capabilities = engine
        .hello(HelloParams {
            protocol_version: PROTOCOL_VERSION.into(),
            client: PeerInfo {
                name: "test".into(),
                version: "0".into(),
            },
        })
        .await
        .expect("hello")
        .capabilities;

    assert!(capabilities.contains(&capability::SESSION_SUBSCRIBE.to_string()));
    let served = engine
        .session_subscribe(
            SessionSubscribeParams {
                session_id: id.clone(),
                from_seq: None,
            },
            Arc::new(Recorder::default()),
        )
        .await;
    assert!(served.is_ok(), "an advertised call must answer");

    assert!(
        !capabilities.contains(&capability::AGENT_INTERRUPT.to_string()),
        "agent.interrupt is not served yet, so it must not be promised"
    );
    assert_eq!(
        engine
            .agent_interrupt(SessionRef { session_id: id })
            .await
            .expect_err("not served yet")
            .kind(),
        Some(ErrorCode::NotImplemented)
    );
}
