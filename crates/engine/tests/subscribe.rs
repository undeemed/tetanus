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
    capability, AgentPromptParams, AgentStatusPush, Engine, EventSink, HelloParams, PeerInfo,
    SessionCreateParams, SessionEventPush, SessionForkParams, SessionRef, SessionSubscribeParams,
    SessionUnsubscribeParams,
};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::PROTOCOL_VERSION;
use tetanus_session::SessionLog;

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<SessionEventPush>>,
    statuses: Mutex<Vec<AgentStatusPush>>,
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

    /// The session named on every frame this sink was handed, of either kind.
    fn sessions(&self) -> Vec<String> {
        let events = self.events.lock().expect("events");
        let statuses = self.statuses.lock().expect("statuses");
        events
            .iter()
            .map(|push| push.session_id.clone())
            .chain(statuses.iter().map(|push| push.session_id.clone()))
            .collect()
    }
}

impl EventSink for Recorder {
    fn session_event(&self, push: SessionEventPush) {
        self.events.lock().expect("events").push(push);
    }
    fn agent_status(&self, push: AgentStatusPush) {
        self.statuses.lock().expect("statuses").push(push);
    }
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

    assert!(capabilities.contains(&capability::SESSION_FORK.to_string()));
    assert!(
        engine
            .session_fork(SessionForkParams {
                session_id: id.clone(),
                through_seq: None,
                child_session_id: None,
            })
            .await
            .is_ok(),
        "an advertised call must answer"
    );

    assert!(capabilities.contains(&capability::AGENT_INTERRUPT.to_string()));
    assert!(
        engine
            .agent_interrupt(SessionRef { session_id: id })
            .await
            .is_ok(),
        "an advertised call must answer"
    );

    // The other direction: nothing else is promised. `ui.ask` is a client
    // capability, so a server that advertised it would be claiming to be a
    // surface.
    assert_eq!(
        capabilities,
        vec![
            capability::SESSION_SUBSCRIBE.to_string(),
            capability::SESSION_FORK.to_string(),
            capability::AGENT_INTERRUPT.to_string(),
        ]
    );
}

/// TC-SUB-6: contract §4.4.5. `from_seq` on a subscription is a seq and is
/// inclusive, and one past the tail replays nothing rather than failing.
///
/// Input: a journal of four events (seqs 0..=3), subscribed twice: from seq 2,
/// and from a seq the journal never reached.
/// Expected: the first replays seqs 2 and 3 and no earlier one; the second
/// replays nothing. Both report `last_seq: 3`, the true tail, so a caller that
/// over-asked still learns where live delivery begins.
#[tokio::test]
async fn from_seq_is_inclusive_and_a_seq_past_the_tail_replays_nothing() {
    let (engine, _dir) = engine();
    let id = session(&engine).await;
    for n in 1..=3 {
        append(&engine, &id, n);
    }

    let midway = Arc::new(Recorder::default());
    let result = engine
        .session_subscribe(
            SessionSubscribeParams {
                session_id: id.clone(),
                from_seq: Some(2),
            },
            midway.clone(),
        )
        .await
        .expect("subscribe");
    assert_eq!(midway.seqs(), vec![2, 3], "the named seq replays too");
    assert_eq!(result.last_seq, 3);

    let over = Arc::new(Recorder::default());
    let result = engine
        .session_subscribe(
            SessionSubscribeParams {
                session_id: id.clone(),
                from_seq: Some(99),
            },
            over.clone(),
        )
        .await
        .expect("a seq past the tail is not an error");
    assert!(over.seqs().is_empty(), "there was nothing to catch up on");
    assert_eq!(result.last_seq, 3, "the true tail, not the seq asked for");

    append(&engine, &id, 4);
    assert_eq!(midway.seqs(), vec![2, 3, 4]);
    assert_eq!(over.seqs(), vec![4], "live delivery starts at the tail");
}

/// TC-SUB-7: contract §4.4.5. A push reaches only the subscriptions on its own
/// session, for both frames.
///
/// Input: one engine, two sessions, one subscription on each, and a whole
/// prompt run on the first.
/// Expected: every frame the second sink was handed - `session/event` and
/// `agent/status` alike - is empty, while the first sink saw both kinds. One
/// connection may hold subscriptions on several sessions, so a sink that saw
/// another session's traffic would be leaking it to that connection.
#[tokio::test]
async fn a_push_reaches_only_its_own_session() {
    let (engine, _dir) = engine();
    let mine = session(&engine).await;
    let other = session(&engine).await;

    let here = Arc::new(Recorder::default());
    let there = Arc::new(Recorder::default());
    for (id, sink) in [(&mine, &here), (&other, &there)] {
        engine
            .session_subscribe(
                SessionSubscribeParams {
                    session_id: id.clone(),
                    from_seq: None,
                },
                sink.clone(),
            )
            .await
            .expect("subscribe");
    }

    engine
        .agent_prompt(AgentPromptParams {
            session_id: mine.clone(),
            content: "please answer".into(),
        })
        .await
        .expect("the turn ran");

    assert!(
        !here.seqs().is_empty(),
        "the turn's own subscription saw its events"
    );
    assert!(
        !here.statuses.lock().expect("statuses").is_empty(),
        "the turn's own subscription saw its status"
    );
    assert!(
        here.sessions().iter().all(|id| *id == mine),
        "a sink saw a frame for a session it did not subscribe to"
    );
    assert!(
        there.sessions().is_empty(),
        "the other session's subscription saw {:?}",
        there.sessions()
    );
}
