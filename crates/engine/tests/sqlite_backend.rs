//! Test Design Specification: the engine over the SQLite session backend.
//!
//! Features under test: `sessions.backend`, and the claim the whole backend
//! seam exists for - every `session.*` call answers the same way whichever
//! artifact the journals live in. Upstream's peer is
//! `packages/session/session-persistence-sqlite`, whose backend sits under the
//! same persistence coordinator its JSONL peer does.
//!
//! Approach: the same script driven twice through `HarnessEngine`, once over
//! each backend, and the two answers compared. A field that cannot agree by
//! construction - the created time, which is a clock reading, and the path,
//! which names a different artifact on purpose - is set aside explicitly
//! rather than left out of the comparison silently.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tempfile::TempDir;
use tetanus_engine::session::{SessionBackend, BACKEND_SQLITE, SQLITE_FILE};
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    Engine, SessionCreateParams, SessionEventsParams, SessionForkParams,
};
use tetanus_protocol::types::SessionInfo;

fn engine_over(dir: &TempDir, backend: SessionBackend) -> HarnessEngine {
    HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        sessions_backend: backend,
        ..EngineConfig::default()
    })
}

fn sqlite_engine(dir: &TempDir) -> HarnessEngine {
    let backend = SessionBackend::named(BACKEND_SQLITE, dir.path()).expect("open the database");
    engine_over(dir, backend)
}

/// One session's worth of durable facts, appended through the live log so the
/// script is identical on both backends.
async fn script(engine: &HarnessEngine, id: &str) {
    engine
        .session_create(SessionCreateParams {
            session_id: Some(id.into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    let live = engine.sessions().live(id).expect("live");
    live.log
        .append("turn/start", serde_json::json!({ "turn": 1 }))
        .expect("append");
    live.log
        .append_with_sources(
            "user/message",
            serde_json::json!({ "content": "what does this repository do?" }),
            vec![],
        )
        .expect("append");
    live.log
        .append("turn/end", serde_json::json!({ "turn": 1 }))
        .expect("append");
    live.log.flush().expect("flush");
}

/// The facts of a listing that are the session's own, rather than the
/// artifact's or the clock's.
fn comparable(info: &SessionInfo) -> (String, String, String, i64, Option<String>) {
    (
        info.session_id.clone(),
        info.provider.clone(),
        info.model.clone(),
        info.last_seq,
        info.title.clone(),
    )
}

/// TC-PORT-STORE-Q15: the engine answers the same over either backend.
///
/// The acceptance claim, stated where a surface can see it: a session created,
/// written and listed through the contract reads identically whichever
/// artifact holds it.
///
/// Expected: `session.list` reports the same id, route, last seq and title on
/// both, and `session.events` returns the same page.
#[tokio::test]
async fn the_engine_answers_the_same_over_either_backend() {
    let jsonl_dir = TempDir::new().expect("temp dir");
    let sqlite_dir = TempDir::new().expect("temp dir");

    let jsonl = engine_over(&jsonl_dir, SessionBackend::Jsonl);
    let sqlite = sqlite_engine(&sqlite_dir);
    script(&jsonl, "twin").await;
    script(&sqlite, "twin").await;

    let left = jsonl.session_list().await.expect("list");
    let right = sqlite.session_list().await.expect("list");
    assert_eq!(
        left.sessions.iter().map(comparable).collect::<Vec<_>>(),
        right.sessions.iter().map(comparable).collect::<Vec<_>>(),
    );

    let params = || SessionEventsParams {
        session_id: "twin".into(),
        from_seq: 0,
        limit: None,
    };
    let left = jsonl.session_events(params()).await.expect("events");
    let right = sqlite.session_events(params()).await.expect("events");
    assert_eq!(left.eof, right.eof);
    assert_eq!(left.next_seq, right.next_seq);
    assert_eq!(
        left.events
            .iter()
            .map(|e| (e.ty.clone(), e.seq, e.data.clone()))
            .collect::<Vec<_>>(),
        right
            .events
            .iter()
            .map(|e| (e.ty.clone(), e.seq, e.data.clone()))
            .collect::<Vec<_>>(),
    );
}

/// TC-PORT-STORE-Q16: a SQLite-backed store keeps one artifact, not a directory.
///
/// Expected: the root holds the database and no `.jsonl` file, and every
/// session reports the database as its path.
#[tokio::test]
async fn a_sqlite_store_keeps_one_artifact() {
    let dir = TempDir::new().expect("temp dir");
    let engine = sqlite_engine(&dir);
    script(&engine, "alpha").await;
    script(&engine, "beta").await;

    let journals: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read root")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".jsonl"))
        .collect();
    assert!(journals.is_empty(), "no journal files: {journals:?}");

    let listed = engine.session_list().await.expect("list");
    let database = dir.path().join(SQLITE_FILE).display().to_string();
    assert_eq!(listed.sessions.len(), 2);
    for session in &listed.sessions {
        assert_eq!(session.path, database);
    }
}

/// TC-PORT-STORE-Q17: a restart finds every session the database holds.
///
/// The cold-read path: a second engine over the same root, with nothing in
/// memory, lists and pages the sessions the first one wrote.
///
/// Expected: both ids are listed with their events, and one is reopened
/// continuing its numbering rather than starting a second history.
#[tokio::test]
async fn a_restart_finds_every_session_in_the_database() {
    let dir = TempDir::new().expect("temp dir");
    {
        let engine = sqlite_engine(&dir);
        script(&engine, "alpha").await;
        script(&engine, "beta").await;
    }

    let restarted = sqlite_engine(&dir);
    let listed = restarted.session_list().await.expect("list");
    let ids: Vec<_> = listed
        .sessions
        .iter()
        .map(|s| s.session_id.clone())
        .collect();
    assert_eq!(ids, vec!["alpha".to_string(), "beta".to_string()]);
    assert!(listed.sessions.iter().all(|s| s.last_seq == 3));

    let reopened = restarted
        .session_create(SessionCreateParams {
            session_id: Some("alpha".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("reopen");
    assert_eq!(reopened.last_seq, 3, "the stored history was not replaced");
}

/// TC-PORT-STORE-Q18: forking works over the database too.
///
/// A fork is a seed, and the seed is the one write that is not an append, so
/// it is the write a second backend is most likely to get wrong.
///
/// Expected: the child holds the inherited prefix with its own header, and the
/// parent is unchanged.
#[tokio::test]
async fn a_fork_seeds_a_child_inside_the_database() {
    let dir = TempDir::new().expect("temp dir");
    let engine = sqlite_engine(&dir);
    script(&engine, "parent").await;

    let child = engine
        .session_fork(SessionForkParams {
            session_id: "parent".into(),
            child_session_id: Some("child".into()),
            through_seq: None,
        })
        .await
        .expect("fork");

    assert_eq!(child.session_id, "child");
    assert_eq!(child.last_seq, 3);
    let events = engine
        .session_events(SessionEventsParams {
            session_id: "child".into(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events");
    assert_eq!(events.events[0].ty, "session/start");
    assert_eq!(
        events.events[0].data.get("parent_session"),
        Some(&serde_json::json!("parent"))
    );
    assert_eq!(
        events.events[2].data.get("content").unwrap(),
        "what does this repository do?"
    );

    let parent = engine
        .session_events(SessionEventsParams {
            session_id: "parent".into(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events");
    assert_eq!(parent.events.len(), 4, "the parent gained nothing");
}

/// TC-PORT-STORE-Q19: a path names a file, so a database store refuses one.
///
/// A session inside a database is named by its id. Answering a `path` with
/// some other session's log, or quietly ignoring it, is worse than refusing:
/// a caller that named a file expects that file.
///
/// Expected: `session.create` with a path is `InvalidParams` naming the field.
#[tokio::test]
async fn a_database_store_refuses_a_named_path() {
    let dir = TempDir::new().expect("temp dir");
    let engine = sqlite_engine(&dir);

    let refused = engine
        .session_create(SessionCreateParams {
            path: Some(dir.path().join("elsewhere.jsonl").display().to_string()),
            ..SessionCreateParams::default()
        })
        .await
        .expect_err("a path is not an id");

    assert_eq!(
        refused.code,
        tetanus_protocol::rpc::ErrorCode::InvalidParams as i32
    );
    assert_eq!(
        refused.data.as_ref().and_then(|d| d.get("field")),
        Some(&serde_json::json!("path"))
    );
}

/// TC-PORT-STORE-Q20: an unserved backend name is a boot fault.
///
/// Expected: `SessionBackend::named` refuses the name and says which two this
/// build serves, so the message is actionable without reading the source.
#[test]
fn an_unserved_backend_name_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let refused = SessionBackend::named("postgres", dir.path()).expect_err("not served");
    assert!(refused.contains("postgres"), "{refused}");
    assert!(
        refused.contains("jsonl") && refused.contains("sqlite"),
        "{refused}"
    );
}

/// TC-PORT-STORE-Q21: `config.dump` reports which backend is running.
///
/// A deployment that cannot see which artifact its history is going into
/// cannot tell a misconfiguration from an empty history.
///
/// Expected: `sessions.backend` is in the dump with the running backend's name.
#[tokio::test]
async fn the_dump_reports_the_running_backend() {
    let dir = TempDir::new().expect("temp dir");
    let engine = sqlite_engine(&dir);

    let dump = engine.config_dump().await.expect("dump");
    let entry = dump
        .entries
        .iter()
        .find(|entry| entry.key == "sessions.backend")
        .expect("sessions.backend is reported");
    assert_eq!(entry.value, serde_json::json!(BACKEND_SQLITE));
}
