//! Test Design Specification: upstream `SessionStore` behaviour, ported.
//!
//! Features under test: the store half of upstream
//! `packages/core/session/tests/session.spec.ts`, its `describe('SessionStore')`
//! block, restated against `tetanus_engine::session::SessionStore`. Each case
//! names the upstream case it comes from.
//!
//! Approach: a journal root in a temporary directory, driven through the
//! public engine surfaces. Upstream's store has a three-step registration
//! lifecycle (`prepare`, `enter`, `announce`) with rollback, reentrancy and
//! HMR-disposal cases; tetanus has one `session.create` that opens or reopens,
//! so those cases have no counterpart to restate and stay named in
//! `docs/parity.md` instead.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic escaping a case.
//! TC-PORT-STORE-4 provokes one deliberately and the bus contains it.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tetanus_engine::session::{SessionHeader, SESSION_START};
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{Engine, SessionCreateParams};
use tetanus_session::{SessionEvent, SessionEventDispatch, SessionLog};

/// TC-PORT-STORE-1: a journal left on disk is continued, not started again.
///
/// Upstream: `session.spec.ts`, "rejects duplicate ids and supports seeding".
/// tetanus diverges deliberately: an id that already has a journal is not a
/// duplicate to refuse, it is a session to resume, so the earlier events are
/// the seed. What both must agree on is that the second call never destroys
/// the first call's log.
///
/// Input: a session created, written to, and then reopened by a second store
/// over the same root, so nothing is served from memory.
/// Expected: the reopened session carries the same id, header and events, and
/// its next append continues the numbering.
#[tokio::test]
async fn a_journal_on_disk_is_continued_by_a_second_store() {
    let dir = TempDir::new().expect("temp dir");

    let first = engine_over(&dir);
    let created = first
        .session_create(SessionCreateParams {
            session_id: Some("seeded".into()),
            model: Some("first-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    let live = first.sessions().live("seeded").expect("live");
    live.log
        .append("user/message", serde_json::json!({ "content": "hello" }))
        .expect("append");
    live.log.flush().expect("flush");
    drop(live);
    drop(first);

    let second = engine_over(&dir);
    let reopened = second
        .session_create(SessionCreateParams {
            session_id: Some("seeded".into()),
            model: Some("second-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("reopen");

    assert_eq!(reopened.session_id, created.session_id);
    assert_eq!(reopened.model, "first-model", "the header is the journal's");
    assert_eq!(reopened.last_seq, 1, "the seed survived");
    assert_eq!(reopened.title.as_deref(), Some("hello"));

    let next = second
        .sessions()
        .live("seeded")
        .expect("live")
        .log
        .append("user/message", serde_json::json!({ "content": "again" }))
        .expect("append");
    assert_eq!(next.seq, 2, "numbering continues, it does not restart");
}

/// TC-PORT-STORE-2: a live id is handed back, never overwritten.
///
/// Upstream: `session.spec.ts`, "enter() rejects a stale prepared session
/// whose id is already live (no overwrite)". tetanus answers the same hazard
/// by returning the session that is already there. TC-SESS-2 in `sessions.rs`
/// pins the header a reopen keeps; what matters here is the identity of the
/// handle, because that is what an in-flight turn is holding.
///
/// Input: a second `session.create` for a live id, asking for another model.
/// Expected: the same journal, the same header, and the very same live handle,
/// so no in-flight turn is left holding a session the store has replaced.
#[tokio::test]
async fn a_live_id_is_handed_back_and_not_replaced() {
    let dir = TempDir::new().expect("temp dir");
    let engine = engine_over(&dir);

    let first = engine
        .session_create(SessionCreateParams {
            session_id: Some("busy".into()),
            model: Some("first-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    let held = engine.sessions().live("busy").expect("live");

    let second = engine
        .session_create(SessionCreateParams {
            session_id: Some("busy".into()),
            model: Some("second-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create again");

    assert_eq!(second.model, first.model);
    assert_eq!(second.path, first.path);
    let now = engine.sessions().live("busy").expect("live");
    assert!(
        Arc::ptr_eq(&held, &now),
        "the handle an in-flight turn holds is still the store's"
    );
    assert_eq!(now.log.events().len(), 1, "no second header was appended");
}

/// TC-PORT-STORE-3: a bare create synthesizes its whole header from the
/// defaults, and an explicit field overrides exactly that field.
///
/// Upstream: `session.spec.ts`, "synthesizes a minimal current-version header
/// for a bare-created session".
///
/// Expected: with no params, provider, model and `max_steps` are the store's
/// defaults, written durably as the journal's first line; with `model` named,
/// only the model changes.
#[tokio::test]
async fn a_bare_create_takes_the_whole_header_from_the_defaults() {
    let dir = TempDir::new().expect("temp dir");
    let engine = engine_over(&dir);

    let bare = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create");
    assert_eq!(bare.provider, "test-provider");
    assert_eq!(bare.model, "test-model");
    let header = header_on_disk(&bare.path);
    assert_eq!(header.session_id, bare.session_id);
    assert_eq!(header.max_steps, 3, "the default is durable, not implied");

    let overridden = engine
        .session_create(SessionCreateParams {
            model: Some("another-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    assert_eq!(overridden.model, "another-model");
    assert_eq!(
        overridden.provider, "test-provider",
        "an override is the field it names, not the header"
    );
    assert_eq!(header_on_disk(&overridden.path).max_steps, 3);
}

/// TC-PORT-STORE-4: an observer that fails cannot lose an event the log has
/// committed, nor keep it from the observers behind it.
///
/// Upstream: `session.spec.ts`, "contains session/event observer failures
/// after the append commit point".
///
/// Two rules meet here. The commit point: the journal is written and fsynced,
/// and the in-memory log is grown, before any observer is told. Containment:
/// the bus catches a panicking `emit` observer, so `append` returns normally
/// and the observers behind it still run. `crates/core/tests/containment.rs`
/// states the second rule at bus level; this case states it at the seam that
/// has something durable to lose.
///
/// Input: an observer that panics, a second observer registered behind it,
/// then one append.
/// Expected: the append returns `Ok`, the second observer saw the event, and
/// the event is in memory and on the journal after a replay.
#[tokio::test]
async fn an_observer_failure_cannot_lose_a_committed_event() {
    let dir = TempDir::new().expect("temp dir");
    let engine = engine_over(&dir);
    engine
        .session_create(SessionCreateParams {
            session_id: Some("observed".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    let live = engine.sessions().live("observed").expect("live");

    let _boom = live
        .bus
        .on_emit::<SessionEventDispatch>(|_| panic!("an observer failed"));
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let peer = Arc::clone(&seen);
    let _watch = live.bus.on_emit::<SessionEventDispatch>(move |ev| {
        peer.lock().expect("seen").push(ev.event.ty.clone())
    });

    // The contained panic still runs the default hook, which would print.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    live.log
        .append("user/message", serde_json::json!({ "content": "kept" }))
        .expect("the append is not the observer's to fail");
    std::panic::set_hook(hook);

    assert_eq!(
        seen.lock().expect("seen").clone(),
        vec!["user/message".to_string()],
        "the observer behind the failing one was still told"
    );

    let events = live.log.events();
    assert_eq!(events.len(), 2, "the append committed");
    assert_eq!(events[1].ty, "user/message");

    let replayed = tetanus_session::replay(&live.path).expect("replay");
    assert_eq!(replayed, events, "and it reached the journal");
}

/// TC-PORT-STORE-5: one bus per session, so an observer of one session never
/// sees another session's events.
///
/// Upstream: `session.spec.ts`, "prevents simultaneous attachment of one
/// session object to two stores" - the same concern, that two sessions never
/// share one identity, at the seam tetanus actually has.
///
/// Expected: appending to each of two live sessions delivers exactly that
/// session's event to exactly that session's observer.
#[tokio::test]
async fn each_session_publishes_only_its_own_events() {
    let dir = TempDir::new().expect("temp dir");
    let engine = engine_over(&dir);
    for id in ["left", "right"] {
        engine
            .session_create(SessionCreateParams {
                session_id: Some(id.into()),
                ..SessionCreateParams::default()
            })
            .await
            .expect("create");
    }

    let left = engine.sessions().live("left").expect("live");
    let right = engine.sessions().live("right").expect("live");
    let (seen_left, _l) = watch(&left.bus);
    let (seen_right, _r) = watch(&right.bus);

    left.log
        .append(
            "user/message",
            serde_json::json!({ "content": "to the left" }),
        )
        .expect("append");
    right
        .log
        .append(
            "user/message",
            serde_json::json!({ "content": "to the right" }),
        )
        .expect("append");

    let left_seen = seen_left.lock().expect("seen").clone();
    let right_seen = seen_right.lock().expect("seen").clone();
    assert_eq!(left_seen.len(), 1);
    assert_eq!(right_seen.len(), 1);
    assert_eq!(left_seen[0].data["content"], "to the left");
    assert_eq!(right_seen[0].data["content"], "to the right");
}

/// An engine over one journal root, with defaults no other case's default
/// could be mistaken for.
fn engine_over(dir: &TempDir) -> HarnessEngine {
    HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        default_provider: "test-provider".into(),
        default_model: "test-model".into(),
        max_steps: 3,
        ..EngineConfig::default()
    })
}

/// The header a journal actually holds, read back from its first line.
fn header_on_disk(path: &str) -> SessionHeader {
    let events = tetanus_session::replay(path).expect("replay");
    assert_eq!(events[0].ty, SESSION_START, "the header is the first line");
    serde_json::from_value(events[0].data.clone()).expect("header")
}

/// Collect every event a session's bus publishes. The handle is returned so
/// the caller keeps the registration alive.
fn watch(bus: &tetanus_core::EventBus) -> (Arc<Mutex<Vec<SessionEvent>>>, impl Drop) {
    let seen: Arc<Mutex<Vec<SessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let handle = bus.on_emit::<SessionEventDispatch>(move |ev| {
        sink.lock().expect("seen").push(ev.event.clone());
    });
    (seen, handle)
}
