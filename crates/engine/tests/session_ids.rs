//! Test Design Specification: a session id resolves to the journal it names.
//!
//! Feature under test: contract section 4.7 - "An id is a fact of the journal
//! and not of its file name, so every id `session.list` reports is one that
//! `session.events` and `session.subscribe` resolve, whatever the file holding
//! that journal is called."
//!
//! Approach: drive `session.create`, `session.list` and `session.events`
//! through the `Engine` trait only, against a temporary journal root, so a case
//! sees exactly what a surface sees. Issue #67 is the anomaly these close.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{Engine, SessionCreateParams, SessionEventsParams};

fn engine() -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });
    (engine, dir)
}

/// TC-ID-1: an id minted for a journal whose file is called something else is
/// still an id `session.events` opens.
///
/// Issue #67: `session.create` with a `path` mints `s<millis>` and writes it
/// into a file named after the path, so the two disagree from that moment on.
///
/// Expected: the `session_id` the create answered with pages that journal, and
/// the page holds the header the create wrote.
#[tokio::test]
async fn a_minted_id_opens_the_journal_it_was_minted_for() {
    let (engine, dir) = engine();
    let path = dir.path().join("my-run.jsonl");

    let created = engine
        .session_create(SessionCreateParams {
            path: Some(path.display().to_string()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    assert_ne!(
        created.session_id, "my-run",
        "the id under test is the minted one, not the file stem"
    );

    let page = engine
        .session_events(SessionEventsParams {
            session_id: created.session_id.clone(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("the minted id resolves");
    assert_eq!(page.events[0].ty, "session/start");
    assert_eq!(
        page.events[0].data["session_id"],
        serde_json::json!(created.session_id)
    );
}

/// TC-ID-2: every id `session.list` reports is one `session.events` opens.
///
/// This is the guarantee itself, stated as a loop rather than as one case, so
/// a future listing rule cannot satisfy it for one entry and break it for
/// another.
///
/// Input: a cold root holding three journals - one named after its id, one
/// named after a path, and one whose file was renamed after it was written.
/// Expected: `session.list` reports three sessions, and each reported id pages
/// a journal whose `session/start` carries that same id.
#[tokio::test]
async fn every_listed_id_is_an_id_that_opens() {
    let (engine, dir) = engine();

    engine
        .session_create(SessionCreateParams {
            session_id: Some("named".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create by id");
    engine
        .session_create(SessionCreateParams {
            path: Some(dir.path().join("by-path.jsonl").display().to_string()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create by path");
    engine
        .session_create(SessionCreateParams {
            session_id: Some("renamed".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create to rename");

    // A cold store, so nothing is answered out of the live map, and a file
    // that no longer agrees with the id inside it.
    drop(engine);
    std::fs::rename(
        dir.path().join("renamed.jsonl"),
        dir.path().join("moved.jsonl"),
    )
    .expect("rename");
    let cold = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });

    let listed = cold.session_list().await.expect("list").sessions;
    assert_eq!(listed.len(), 3, "one entry per journal: {listed:?}");

    for session in listed {
        let page = cold
            .session_events(SessionEventsParams {
                session_id: session.session_id.clone(),
                from_seq: 0,
                limit: None,
            })
            .await
            .unwrap_or_else(|e| panic!("listed id `{}` did not open: {e:?}", session.session_id));
        assert_eq!(
            page.events[0].data["session_id"],
            serde_json::json!(session.session_id),
            "the journal an id opens is the journal that claims it"
        );
    }
}

/// TC-ID-3: a live session is listed once, not twice.
///
/// Keying cold journals by file name while reporting the header id listed the
/// same session under both spellings as soon as the two differed.
///
/// Expected: one entry for the one session, and it names the journal's path.
#[tokio::test]
async fn a_live_session_with_a_renamed_file_is_listed_once() {
    let (engine, dir) = engine();
    let path = dir.path().join("held-open.jsonl");
    let created = engine
        .session_create(SessionCreateParams {
            path: Some(path.display().to_string()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");

    let listed = engine.session_list().await.expect("list").sessions;
    assert_eq!(listed.len(), 1, "one journal, one entry: {listed:?}");
    assert_eq!(listed[0].session_id, created.session_id);
    assert_eq!(listed[0].path, path.display().to_string());
}

/// TC-ID-4: a file named `<id>.jsonl` that belongs to another session does not
/// capture that id.
///
/// The file-name fast path must stay a fast path. A journal whose header says
/// `other` cannot answer for the id `decoy` just because of what it is called.
///
/// Expected: `decoy` resolves to the journal that claims `decoy`, and never to
/// the file named `decoy.jsonl`.
#[tokio::test]
async fn a_file_name_does_not_capture_an_id_it_does_not_own() {
    let (engine, dir) = engine();
    engine
        .session_create(SessionCreateParams {
            session_id: Some("decoy".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create decoy");
    engine
        .session_create(SessionCreateParams {
            session_id: Some("other".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create other");
    drop(engine);

    // `other.jsonl` takes the name `decoy.jsonl`, and the real `decoy` journal
    // moves aside. Both still carry their own headers.
    std::fs::rename(
        dir.path().join("decoy.jsonl"),
        dir.path().join("elsewhere.jsonl"),
    )
    .expect("rename decoy");
    std::fs::rename(
        dir.path().join("other.jsonl"),
        dir.path().join("decoy.jsonl"),
    )
    .expect("rename other");

    let cold = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });
    let page = cold
        .session_events(SessionEventsParams {
            session_id: "decoy".into(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("decoy resolves");
    assert_eq!(
        page.events[0].data["session_id"],
        serde_json::json!("decoy"),
        "the header decides, not the file name"
    );
}
