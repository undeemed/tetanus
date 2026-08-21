//! Test Design Specification: driving the harness in process, typed.
//!
//! Feature under test: `tetanus_sdk::Client` and `tetanus_sdk::Harness` - the
//! client library a Rust caller uses instead of the CLI or a carrier. This is
//! upstream's `sdk/*`, which `docs/parity.md` marks phase ③.
//!
//! Approach: every case drives a real `HarnessEngine` on the offline mock
//! adapter through the SDK and nothing else. No case spawns a process, opens a
//! socket, or reaches for `env!("CARGO_BIN_EXE_tetanus")`: the claim being made
//! is that a caller needs none of those, and a case that used one would not be
//! making it. The two cases about the handshake rule use a double, because a
//! refusal is what they are about and a healthy engine does not produce one.
//!
//! Features NOT tested here: the turn itself, which `crates/turn` owns, and the
//! engine's answers to each call, which `crates/engine/tests` owns. This suite
//! asserts what the SDK adds - the handshake, the ordering of a run, the
//! collection, and the lifetime.
//!
//! Environmental needs: a writable temp directory. No case reaches a network or
//! an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    capability, Ack, AgentPromptParams, AgentStatusResult, ConfigDumpResult, Engine, EventSink,
    HelloParams, HelloResult, ModelCatalogResult, PeerInfo, SessionCreateParams,
    SessionEventsParams, SessionEventsResult, SessionForkParams, SessionListResult, SessionRef,
    SessionSubscribeParams, SessionSubscribeResult, SessionUnsubscribeParams, ToolCatalogResult,
};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types::{AgentState, SessionInfo};
use tetanus_sdk::{Client, Harness, SdkError, Update};

fn engine(dir: &TempDir) -> Arc<dyn Engine> {
    Arc::new(HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().join("sessions"),
        ..EngineConfig::default()
    }))
}

/// TC-PORT-SDK-1: the SDK drives a whole turn in process, with no CLI and no
/// carrier involved.
///
/// Input: a real engine, a fresh session, one prompt.
/// Expected: the turn's answer comes back through the summary, and the
/// collected events are the documented mock turn - a `turn/start`, a
/// `tool/call` for `echo`, its `tool/result`, and a `turn/end`. Nothing in this
/// case constructs a process or a socket.
#[tokio::test]
async fn the_sdk_runs_a_whole_turn_in_process() {
    let dir = TempDir::new().expect("temp dir");
    let harness = Harness::new(engine(&dir));
    let session = harness.session().await.expect("session");

    let run = session.run("hello sdk").await.expect("turn");

    assert_eq!(run.summary.turn, 1);
    assert_eq!(run.final_response(), "You said: hello sdk");

    let types: Vec<String> = run.events().into_iter().map(|event| event.ty).collect();
    for expected in ["turn/start", "tool/call", "tool/result", "turn/end"] {
        assert!(
            types.iter().any(|ty| ty == expected),
            "no `{expected}` in {types:?}",
        );
    }

    let calls = run.journal().tool_calls();
    assert_eq!(calls.len(), 1, "the turn made one tool call");
    assert_eq!(calls[0].name, "echo");
    assert_eq!(calls[0].ok, Some(true), "and its result came back");

    harness.close().await;
}

/// TC-PORT-SDK-2: a run subscribes before it prompts, so no event of the turn
/// is missed.
///
/// Input: one turn, collected by `run`, compared against the journal the same
/// turn wrote.
/// Expected: the collected events are exactly the journal's events from
/// `turn/start` onward, in the same order. A subscription opened after the
/// prompt would be missing the front of that list, which is the ordering bug
/// this layer exists to own.
#[tokio::test]
async fn a_run_misses_no_event_because_it_subscribes_first() {
    let dir = TempDir::new().expect("temp dir");
    let harness = Harness::new(engine(&dir));
    let session = harness.session().await.expect("session");

    let run = session.run("hello").await.expect("turn");
    let journal = session.journal().await.expect("journal");

    let collected: Vec<(u64, String)> = run
        .events()
        .into_iter()
        .map(|event| (event.seq, event.ty))
        .collect();
    let durable: Vec<(u64, String)> = journal
        .events()
        .iter()
        .filter(|event| event.turn.is_some())
        .map(|event| (event.seq(), event.ty().to_string()))
        .collect();

    assert_eq!(collected, durable, "every event of the turn, in order");
    assert!(collected.first().is_some_and(|(_, ty)| ty == "turn/start"));

    harness.close().await;
}

/// TC-PORT-SDK-3: the two push kinds arrive on one stream, in their true
/// relative order.
///
/// Input: one turn's collected updates.
/// Expected: the first update is the status going `running`, the last is the
/// status going `idle`, and every journal event of the turn sits between them.
/// Two separate lists could not state that, which is why they are one.
#[tokio::test]
async fn status_and_events_interleave_in_one_ordered_stream() {
    let dir = TempDir::new().expect("temp dir");
    let harness = Harness::new(engine(&dir));
    let session = harness.session().await.expect("session");

    let run = session.run("hello").await.expect("turn");

    let first = run.updates.first().expect("an update");
    let last = run.updates.last().expect("an update");
    assert!(
        matches!(first, Update::Status(status) if status.state == AgentState::Running),
        "the turn announces itself before it does anything: {first:?}",
    );
    assert!(
        matches!(last, Update::Status(status) if status.state == AgentState::Idle),
        "and reports itself finished last: {last:?}",
    );

    let statuses = run.statuses();
    assert_eq!(statuses.len(), 2, "one transition each way");
    assert_eq!(statuses[0].turn, Some(1), "running names the turn");

    let events = run.events();
    assert_eq!(
        events.len() + statuses.len(),
        run.updates.len(),
        "nothing is in the stream that is neither",
    );

    harness.close().await;
}

/// TC-PORT-SDK-4: a call before the handshake is refused, with the same code
/// and the same words a carrier refuses it with.
///
/// Input: `catalog.tools` on a client that has not called `start`, then the
/// same call after it. A call that touches no session is used on purpose, so
/// what the case observes is the handshake rule and not the state of a journal
/// directory.
/// Expected: `SdkError::NotStarted`, which converts to `InvalidRequest` naming
/// `rpc.hello`; and the same call served once the hands are shaken. Contract
/// section 4.4.1 makes the handshake the first call, and an SDK that skipped it
/// would let a caller work against a version it never agreed on - then fail the
/// day that caller moved onto a socket.
#[tokio::test]
async fn a_call_before_the_handshake_is_refused_exactly_as_a_carrier_refuses_it() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(engine(&dir));

    let refused = client.catalog_tools().await.expect_err("ungreeted");
    assert_eq!(refused, SdkError::NotStarted);

    let wire = RpcError::from(refused);
    assert_eq!(wire.kind(), Some(ErrorCode::InvalidRequest));
    assert!(wire.message.contains("rpc.hello"), "{}", wire.message);

    client.start().await.expect("handshake");
    client.catalog_tools().await.expect("served after it");
}

/// TC-PORT-SDK-5: the handshake happens once, however many times it is asked
/// for.
///
/// Input: three `start` calls on a double that counts them.
/// Expected: one `rpc.hello` reaches the engine, and all three answers are the
/// same. `start` is memoized so a caller may make it a precondition of every
/// operation without paying for it.
#[tokio::test]
async fn the_handshake_is_memoized() {
    let counter = Arc::new(Counter::default());
    let client = Client::new(Arc::clone(&counter) as Arc<dyn Engine>);

    let first = client.start().await.expect("handshake");
    let second = client.start().await.expect("handshake");
    client.start().await.expect("handshake");

    assert_eq!(counter.hellos.load(Ordering::Relaxed), 1, "asked once");
    assert_eq!(first, second);
    assert_eq!(client.server(), Some(first));
}

/// TC-PORT-SDK-6: a refused handshake is not memoized, so a caller may correct
/// itself and try again.
///
/// Input: a double that refuses the first `rpc.hello` and accepts the second.
/// Expected: the first `start` fails, the client is still ungreeted so calls
/// are still refused, and the second `start` succeeds. This is what the codec
/// does with a refused hello frame: the handshake is settled by the engine
/// accepting it, not by the frame arriving.
#[tokio::test]
async fn a_refused_handshake_leaves_the_client_ungreeted() {
    let counter = Arc::new(Counter {
        refuse_first: true,
        ..Counter::default()
    });
    let client = Client::new(Arc::clone(&counter) as Arc<dyn Engine>);

    let refused = client.start().await.expect_err("the first is refused");
    assert!(matches!(refused, SdkError::Refused(_)));
    assert_eq!(client.server(), None, "nothing was settled");
    assert_eq!(
        client.session_list().await.expect_err("still ungreeted"),
        SdkError::NotStarted,
    );

    client.start().await.expect("the second is accepted");
    assert!(client.server().is_some());
    assert_eq!(counter.hellos.load(Ordering::Relaxed), 2);
}

/// TC-PORT-SDK-7: the handshake reports which optional calls this build
/// serves, and the client answers about them without a second call.
///
/// Input: a real engine's handshake.
/// Expected: `supports` is true for the three capabilities this build
/// advertises and false for one it does not, and false on a client that has not
/// shaken hands - nothing has been promised yet.
#[tokio::test]
async fn capabilities_are_readable_from_the_client() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(engine(&dir));

    assert!(
        !client.supports(capability::SESSION_SUBSCRIBE),
        "unshaken hands promise nothing",
    );

    client.start().await.expect("handshake");
    assert!(client.supports(capability::SESSION_SUBSCRIBE));
    assert!(client.supports(capability::SESSION_FORK));
    assert!(client.supports(capability::AGENT_INTERRUPT));
    assert!(
        !client.supports(capability::AGENT_STEER),
        "reserved, and this build does not serve it",
    );
}

/// TC-PORT-SDK-8: closing a client closes the subscriptions it opened.
///
/// Input: two subscriptions, then `close`.
/// Expected: both are closed on the engine - a second close of either reports
/// it did no work - and the client refuses every later call. This is the
/// promise a carrier makes when its peer hangs up, and an in-process client
/// with no connection to lose has to make it explicitly.
#[tokio::test]
async fn closing_a_client_closes_what_it_opened_and_is_terminal() {
    let dir = TempDir::new().expect("temp dir");
    let raw = engine(&dir);
    let client = Client::new(Arc::clone(&raw));
    client.start().await.expect("handshake");
    let info = client
        .session_create(SessionCreateParams::default())
        .await
        .expect("session");

    let mut ids = Vec::new();
    for _ in 0..2 {
        let subscription = client
            .session_subscribe(SessionSubscribeParams {
                session_id: info.session_id.clone(),
                from_seq: None,
            })
            .await
            .expect("subscribe");
        ids.push(subscription.id().to_string());
    }

    client.close().await;

    for id in ids {
        let again = raw
            .session_unsubscribe(SessionUnsubscribeParams {
                subscription_id: id.clone(),
            })
            .await
            .expect("closing a closed subscription is not an error");
        assert!(!again.ok, "`{id}` was already closed by the client");
    }

    assert_eq!(
        client.session_list().await.expect_err("closed"),
        SdkError::Closed,
    );
    assert_eq!(client.start().await.expect_err("closed"), SdkError::Closed);
    client.close().await;
    assert!(client.is_closed(), "closing twice is not an error");
}

/// TC-PORT-SDK-9: a subscription delivers live events and stops when it is
/// closed.
///
/// Input: a subscription, one turn, then a close, then a second turn.
/// Expected: the first turn's events arrive; after the close nothing more
/// does. A sink the engine keeps writing to after its reader is gone is the
/// leak this asserts against.
#[tokio::test]
async fn a_closed_subscription_stops_delivering() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(engine(&dir));
    client.start().await.expect("handshake");
    let info = client
        .session_create(SessionCreateParams::default())
        .await
        .expect("session");

    let mut subscription = client
        .session_subscribe(SessionSubscribeParams {
            session_id: info.session_id.clone(),
            from_seq: None,
        })
        .await
        .expect("subscribe");
    assert_eq!(subscription.last_seq(), 0, "starts after the header");

    client
        .agent_prompt(AgentPromptParams {
            session_id: info.session_id.clone(),
            content: "one".into(),
        })
        .await
        .expect("turn");
    assert!(!subscription.drain().is_empty(), "the turn was delivered");

    let id = subscription.id().to_string();
    client.session_unsubscribe(&id).await.expect("unsubscribe");

    client
        .agent_prompt(AgentPromptParams {
            session_id: info.session_id.clone(),
            content: "two".into(),
        })
        .await
        .expect("turn");
    assert!(
        subscription.drain().is_empty(),
        "a closed subscription receives nothing",
    );
}

/// TC-PORT-SDK-10: the engine's refusal reaches the caller whole.
///
/// Input: a prompt naming a session nobody created.
/// Expected: `SdkError::Refused` carrying the contract's own
/// `SessionNotFound`, code and all. The SDK does not remap engine failures: the
/// contract's error table is the caller's documentation and a second
/// vocabulary over it would be a second thing to keep true.
#[tokio::test]
async fn an_engine_refusal_is_carried_through_unchanged() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(engine(&dir));
    client.start().await.expect("handshake");

    let refused = client
        .agent_prompt(AgentPromptParams {
            session_id: "no-such-session".into(),
            content: "hi".into(),
        })
        .await
        .expect_err("no such session");

    let SdkError::Refused(error) = refused else {
        panic!("expected the engine's own refusal, got {refused:?}");
    };
    assert_eq!(error.kind(), Some(ErrorCode::SessionNotFound));
}

/// TC-PORT-SDK-11: a session handle answers about itself between turns.
///
/// Input: two turns on one session, with a status read either side.
/// Expected: the session is idle before and after; the second turn is turn 2;
/// and the journal read through the handle holds both turns with the whole
/// session's cost. A caller needs no session id and no second client for any
/// of it.
#[tokio::test]
async fn a_session_handle_carries_the_session() {
    let dir = TempDir::new().expect("temp dir");
    let harness = Harness::new(engine(&dir));
    let session = harness.session().await.expect("session");

    assert_eq!(session.status().await.expect("status"), AgentState::Idle);
    let first = session.run("one").await.expect("turn");
    let second = session.run("two").await.expect("turn");
    assert_eq!(session.status().await.expect("status"), AgentState::Idle);

    assert_eq!(first.summary.turn, 1);
    assert_eq!(second.summary.turn, 2);
    assert_eq!(second.session_id, session.id());

    let journal = session.journal().await.expect("journal");
    assert_eq!(journal.turns().len(), 2);
    assert!(
        journal
            .cost(tetanus_sdk::query::Bound::all())
            .total_tokens()
            > 0
    );

    harness.close().await;
}

/// TC-PORT-SDK-12: a reserved call is routed, not unknown.
///
/// Input: `approval.set` against a build that does not serve it.
/// Expected: `NotImplemented` naming the method - contract section 4.2's
/// answer for a reserved call - rather than `MethodNotFound`. A caller must be
/// able to tell "the contract has this and this build does not serve it" from
/// "no such call".
#[tokio::test]
async fn a_reserved_call_answers_not_implemented_rather_than_unknown() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(engine(&dir));
    client.start().await.expect("handshake");

    let refused = client
        .approval_set(tetanus_protocol::methods::ApprovalSetParams {
            session_id: "s".into(),
            policy: tetanus_protocol::types::ApprovalPolicy::Ask,
        })
        .await
        .expect_err("reserved");

    let SdkError::Refused(error) = refused else {
        panic!("expected a routed refusal, got {refused:?}");
    };
    assert_eq!(error.kind(), Some(ErrorCode::NotImplemented));
    assert_eq!(
        error.data,
        Some(serde_json::json!({ "method": "approval.set" })),
    );
}

// ---- doubles ---------------------------------------------------------------

/// An engine that counts handshakes and can refuse the first one.
#[derive(Default)]
struct Counter {
    hellos: AtomicUsize,
    refuse_first: bool,
}

fn unused<T>() -> T {
    unreachable!("no case in this file makes this call")
}

#[async_trait::async_trait]
impl Engine for Counter {
    async fn hello(&self, _: HelloParams) -> Result<HelloResult, RpcError> {
        let nth = self.hellos.fetch_add(1, Ordering::Relaxed);
        if self.refuse_first && nth == 0 {
            return Err(RpcError::new(
                ErrorCode::UnsupportedProtocolVersion,
                "not this version",
            ));
        }
        Ok(HelloResult {
            protocol_version: tetanus_protocol::PROTOCOL_VERSION.into(),
            server: PeerInfo {
                name: "double".into(),
                version: "0".into(),
            },
            capabilities: Vec::new(),
        })
    }
    async fn session_list(&self) -> Result<SessionListResult, RpcError> {
        Ok(SessionListResult {
            sessions: Vec::new(),
        })
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
    async fn session_fork(&self, _: SessionForkParams) -> Result<SessionInfo, RpcError> {
        unused()
    }
    async fn session_subscribe(
        &self,
        _: SessionSubscribeParams,
        _: Arc<dyn EventSink>,
    ) -> Result<SessionSubscribeResult, RpcError> {
        unused()
    }
    async fn session_unsubscribe(&self, _: SessionUnsubscribeParams) -> Result<Ack, RpcError> {
        unused()
    }
    async fn agent_prompt(
        &self,
        _: AgentPromptParams,
    ) -> Result<tetanus_protocol::methods::AgentPromptResult, RpcError> {
        unused()
    }
    async fn agent_status(&self, _: SessionRef) -> Result<AgentStatusResult, RpcError> {
        unused()
    }
    async fn agent_interrupt(&self, _: SessionRef) -> Result<Ack, RpcError> {
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
