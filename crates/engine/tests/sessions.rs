//! Conformance for the session surfaces: `session.create`, `session.list`
//! and `session.events`.
//!
//! Test design: each case names the contract clause it fixes. Every case runs
//! offline against a temporary journal root, so none needs a key or a network.

use tempfile::TempDir;
use tetanus_engine::session::MAX_TITLE;
use tetanus_engine::session::{MAX_PAGE, SESSION_START};
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{Engine, SessionCreateParams, SessionEventsParams};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::types::AgentState;
use tetanus_session::SessionLog;

fn engine() -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });
    (engine, dir)
}

/// TC-SESS-1: a fresh session opens an empty journal whose first line is the
/// header, so a cold reader needs no sidecar file.
#[tokio::test]
async fn a_new_session_opens_with_its_header() {
    let (engine, dir) = engine();
    let info = engine
        .session_create(SessionCreateParams {
            model: Some("some-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");

    assert_eq!(info.model, "some-model");
    assert_eq!(info.last_seq, 0, "the header is the only event");
    assert_eq!(info.state, AgentState::Idle);
    assert_eq!(
        info.path,
        dir.path()
            .join(format!("{}.jsonl", info.session_id))
            .display()
            .to_string()
    );
}

/// TC-SESS-2: reopening an id keeps the header the journal was created with.
/// The model a turn already ran under is a fact of the log, not of the call.
#[tokio::test]
async fn reopening_a_session_keeps_its_original_header() {
    let (engine, _dir) = engine();
    let first = engine
        .session_create(SessionCreateParams {
            session_id: Some("resume-me".into()),
            model: Some("first-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");

    let second = engine
        .session_create(SessionCreateParams {
            session_id: Some("resume-me".into()),
            model: Some("second-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("reopen");

    assert_eq!(second.model, "first-model");
    assert_eq!(second.session_id, first.session_id);
    assert_eq!(second.last_seq, 0, "reopening appends no second header");
}

/// TC-SESS-6: reopening a journal a crash left mid-turn closes that turn
/// first, so the session a surface resumes has no dangling tool call.
///
/// Upstream does the same on load (`session-persistence` `prepareCore`); the
/// synthesis itself is pinned by `crates/turn/tests/upstream_repair.rs`.
#[tokio::test]
async fn reopening_an_interrupted_journal_closes_its_turn() {
    let dir = TempDir::new().expect("temp dir");
    let config = EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    };
    let path = dir.path().join("crashed.jsonl");

    // A crash: the boundaries and the model's request were written, the tool
    // result never was, and the process went away without a `turn/end`.
    let crashed = HarnessEngine::new(config.clone());
    crashed
        .session_create(SessionCreateParams {
            session_id: Some("crashed".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    drop(crashed);

    let log =
        tetanus_session::JsonlSessionLog::create("crashed", &path, tetanus_core::EventBus::new())
            .expect("journal");
    log.append("turn/start", serde_json::json!({ "turn": 1 }))
        .expect("append");
    log.append("step/start", serde_json::json!({ "turn": 1, "step": 1 }))
        .expect("append");
    log.append(
        "assistant/message",
        serde_json::json!({
            "content": "",
            "tool_calls": [{ "id": "call-1", "name": "echo", "arguments": {} }],
        }),
    )
    .expect("append");
    log.flush().expect("flush");
    drop(log);

    let restarted = HarnessEngine::new(config);
    let reopened = restarted
        .session_create(SessionCreateParams {
            session_id: Some("crashed".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("reopen");

    let events = tetanus_session::replay(&path).expect("replay");
    let tail: Vec<&str> = events.iter().skip(4).map(|e| e.ty.as_str()).collect();
    assert_eq!(tail, vec!["tool/result", "step/end", "turn/end"]);
    assert_eq!(events[6].data["stop_reason"], "interrupted");
    assert_eq!(
        reopened.last_seq, 6,
        "the reopened session reports the repaired tail"
    );
}

/// TC-SESS-3: a cold journal left behind by a restart is listed from its own
/// header, with no live handle involved.
#[tokio::test]
async fn list_finds_cold_journals() {
    let dir = TempDir::new().expect("temp dir");
    let config = EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    };

    let cold = HarnessEngine::new(config.clone());
    let created = cold
        .session_create(SessionCreateParams {
            model: Some("cold-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    drop(cold);

    let restarted = HarnessEngine::new(config);
    let listed = restarted.session_list().await.expect("list").sessions;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_id, created.session_id);
    assert_eq!(listed[0].model, "cold-model");
    assert_eq!(listed[0].last_seq, 0);
}

/// TC-SESS-4: an id that would reach outside the journal root is
/// `InvalidParams`, never a filesystem error.
#[tokio::test]
async fn ids_that_name_a_path_are_rejected() {
    let (engine, _dir) = engine();

    for id in ["../escape", "with/slash", "..", ""] {
        let error = engine
            .session_create(SessionCreateParams {
                session_id: Some(id.into()),
                ..SessionCreateParams::default()
            })
            .await
            .expect_err("an id that names a path outside the root must be refused");
        assert_eq!(
            error.kind(),
            Some(ErrorCode::InvalidParams),
            "`{id}` must be InvalidParams"
        );
        assert_eq!(
            error.data.expect("data")["field"],
            serde_json::json!("session_id")
        );
    }
}

/// TC-PAGE-1: a durable event crosses the boundary with the journal's own
/// field names, asserted against literal JSON rather than against the
/// converter that produced it.
#[tokio::test]
async fn a_session_event_crosses_the_boundary_unchanged() {
    let (engine, _dir) = engine();
    let info = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create");

    let page = engine
        .session_events(SessionEventsParams {
            session_id: info.session_id.clone(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events");

    let first = serde_json::to_value(&page.events[0]).expect("serialize");
    assert_eq!(first["type"], serde_json::json!(SESSION_START));
    assert_eq!(first["seq"], serde_json::json!(0));
    assert_eq!(
        first["data"]["session_id"],
        serde_json::json!(info.session_id)
    );
    assert!(
        first.get("sourceEventSeqs").is_none(),
        "a header cites nothing"
    );
}

/// TC-PAGE-2: paging is by seq. A page reports the seq the next one starts at,
/// and only the last page is `eof`.
#[tokio::test]
async fn events_page_by_seq() {
    let (engine, _dir) = engine();
    let info = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create");
    let live = engine.sessions().live(&info.session_id).expect("live");
    for n in 0..4 {
        tetanus_session::SessionLog::append(
            live.log.as_ref(),
            "turn/start",
            serde_json::json!({ "turn": n }),
        )
        .expect("append");
    }

    let page = engine
        .session_events(SessionEventsParams {
            session_id: info.session_id.clone(),
            from_seq: 1,
            limit: Some(2),
        })
        .await
        .expect("first page");
    assert_eq!(page.events.len(), 2);
    assert_eq!(page.events[0].seq, 1);
    assert_eq!(page.next_seq, 3);
    assert!(!page.eof);

    let rest = engine
        .session_events(SessionEventsParams {
            session_id: info.session_id,
            from_seq: page.next_seq,
            limit: Some(MAX_PAGE),
        })
        .await
        .expect("last page");
    assert_eq!(rest.events.len(), 2);
    assert!(rest.eof);
}

/// TC-PAGE-3: a limit above the server's maximum is clamped, not refused,
/// which is what the contract's "server clamps to its own maximum" promises.
#[tokio::test]
async fn an_oversized_limit_is_clamped() {
    let (engine, _dir) = engine();
    let info = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create");

    let page = engine
        .session_events(SessionEventsParams {
            session_id: info.session_id,
            from_seq: 0,
            limit: Some(MAX_PAGE * 100),
        })
        .await
        .expect("events");
    assert_eq!(page.events.len(), 1);
    assert!(page.eof);
}

/// TC-PAGE-4: a cold journal pages from disk, so a surface can replay a
/// session this process never opened.
#[tokio::test]
async fn a_cold_journal_pages_from_disk() {
    let dir = TempDir::new().expect("temp dir");
    let config = EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    };

    let cold = HarnessEngine::new(config.clone());
    let created = cold
        .session_create(SessionCreateParams::default())
        .await
        .expect("create");
    drop(cold);

    let page = HarnessEngine::new(config)
        .session_events(SessionEventsParams {
            session_id: created.session_id.clone(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events");
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].ty, SESSION_START);
    assert!(page.eof);
}

/// TC-PAGE-5: an unknown session is `SessionNotFound`, and an id that would
/// reach outside the journal root is `InvalidParams`, never a filesystem
/// error leaking through the boundary.
#[tokio::test]
async fn paging_a_session_that_is_not_there_is_the_documented_code() {
    let (engine, _dir) = engine();

    let missing = engine
        .session_events(SessionEventsParams {
            session_id: "nope".into(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect_err("unknown session");
    assert_eq!(missing.kind(), Some(ErrorCode::SessionNotFound));
    assert_eq!(
        missing.data.expect("data")["session_id"],
        serde_json::json!("nope")
    );

    let escape = engine
        .session_events(SessionEventsParams {
            session_id: "../escape".into(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect_err("an id that names a path outside the root");
    assert_eq!(escape.kind(), Some(ErrorCode::InvalidParams));
}

/// TC-PATH-1: contract §4.7. Naming a path opens the journal there, and the
/// id comes from the journal's own header, so a path becomes an id every
/// other call accepts.
#[tokio::test]
async fn a_named_path_opens_the_journal_there() {
    let (engine, dir) = engine();
    let elsewhere = dir.path().join("nested").join("mine.jsonl");

    let made = engine
        .session_create(SessionCreateParams {
            path: Some(elsewhere.display().to_string()),
            session_id: Some("by-path".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create at a path");
    assert_eq!(made.session_id, "by-path");
    assert_eq!(made.path, elsewhere.display().to_string());
    assert!(
        elsewhere.exists(),
        "the journal is written where it was named"
    );

    // A second engine knows nothing about this journal until it is named.
    let reader = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });
    let reopened = reader
        .session_create(SessionCreateParams {
            path: Some(elsewhere.display().to_string()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("reopen by path alone");
    assert_eq!(
        reopened.session_id, "by-path",
        "the id is read from the journal, not invented"
    );
}

/// TC-PATH-2: `title` is the first user message, cut to one line, and absent
/// until there is one. A surface renders "no title" rather than a guess.
#[tokio::test]
async fn title_is_the_first_user_message() {
    let (engine, _dir) = engine();
    let info = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create");
    assert_eq!(info.title, None, "a session with no prompt has no title");

    let live = engine.sessions().live(&info.session_id).expect("live");
    let long = "x".repeat(MAX_TITLE + 10);
    for content in [format!("{long}\nsecond line"), "later".into()] {
        tetanus_session::SessionLog::append(
            live.log.as_ref(),
            "user/message",
            serde_json::json!({ "content": content }),
        )
        .expect("append");
    }

    let listed = engine.session_list().await.expect("list").sessions;
    let title = listed[0].title.clone().expect("a title");
    assert_eq!(title, format!("{}...", "x".repeat(MAX_TITLE)));
    assert!(!title.contains('\n'), "a title is one line");
}

/// TC-PATH-3: contract §4.7. A path whose file is not a journal is
/// `LogCorrupt`, not a silent empty session and not an IO error.
#[tokio::test]
async fn a_path_that_is_not_a_journal_is_corrupt() {
    let (engine, dir) = engine();
    let junk = dir.path().join("notes.jsonl");
    std::fs::write(&junk, "this is not a journal\n").expect("write");

    let error = engine
        .session_create(SessionCreateParams {
            path: Some(junk.display().to_string()),
            ..SessionCreateParams::default()
        })
        .await
        .expect_err("not a journal");
    assert_eq!(error.kind(), Some(ErrorCode::LogCorrupt));
    assert_eq!(error.data.expect("data")["line"], serde_json::json!(1));
}
