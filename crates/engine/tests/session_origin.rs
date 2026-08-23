//! Test Design Specification: what a journal says about where it came from.
//!
//! Feature under test: contract section 4.4.9's four origin facts, produced by
//! the engine rather than merely representable at the boundary.
//!
//! Why this suite exists. §4.4.9 is fully published: the table in §4.3.1 lists
//! `cwd`, `spawned_by` and `depth`, the section explains each one at length,
//! `KnownEvent::SessionStart` declares all three, and TC-PROTO-30, -31 and -32
//! pass. Every one of those cases builds a `SessionEvent` by hand. Nothing
//! asserted that the engine *writes* any of them, and it did not - a `cwd`
//! field the contract says a journal is unreadable without was absent from
//! every journal tetanus had ever written. This is the same class of defect
//! the compaction lane found on `assistant/message`: a promise with a type and
//! no producer.
//!
//! Approach: real journals, written by the real engine through
//! `session.create` and `session.fork`, read back off disk and parsed through
//! the published boundary type - so a case fails if the engine stops writing
//! a field *or* if the boundary stops reading it.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tempfile::TempDir;
use tetanus_engine::session::{SessionOrigin, SESSION_START};
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{Engine, SessionCreateParams, SessionForkParams};
use tetanus_protocol::types::KnownEvent;
use tetanus_session::SessionEvent;

fn engine_with(dir: &TempDir, origin: SessionOrigin) -> HarnessEngine {
    HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        session_origin: origin,
        ..EngineConfig::default()
    })
}

/// The header line of a journal, as the boundary type reads it.
fn header_of(engine: &HarnessEngine, id: &str) -> KnownEvent {
    let live = engine.sessions().live(id).expect("live");
    let first: SessionEvent = live.log.events().into_iter().next().expect("a header");
    assert_eq!(first.ty, SESSION_START);
    let wire = tetanus_protocol::types::SessionEvent {
        ty: first.ty,
        seq: first.seq,
        time: first.time,
        data: first.data,
        source_event_seqs: first.source_event_seqs,
    };
    wire.parse().expect("the published type reads it")
}

/// TC-ORIGIN-1: a journal records the directory it was opened in.
///
/// §4.4.9: "a journal full of relative paths is unreadable without it, and
/// 'it worked on my machine' is usually a question about this field."
///
/// Expected: `cwd` is present on the header the engine wrote, and it is the
/// directory the run was opened in.
#[tokio::test]
async fn a_journal_records_the_directory_it_was_opened_in() {
    let dir = TempDir::new().expect("temp dir");
    let opened_in = TempDir::new().expect("temp dir");
    let engine = engine_with(
        &dir,
        SessionOrigin {
            cwd: Some(opened_in.path().to_path_buf()),
            ..SessionOrigin::default()
        },
    );
    engine
        .session_create(SessionCreateParams {
            session_id: Some("rooted".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");

    match header_of(&engine, "rooted") {
        KnownEvent::SessionStart { cwd, .. } => assert_eq!(
            cwd.as_deref(),
            Some(opened_in.path().display().to_string().as_str())
        ),
        other => panic!("expected a header, got {other:?}"),
    }
}

/// TC-ORIGIN-2: an ordinary run records the process's own directory.
///
/// The configured directory of TC-ORIGIN-1 is what a test and a container use;
/// this is what every other run gets, and it is the path that actually has to
/// work.
///
/// Expected: `cwd` is present and equals the process's working directory.
#[tokio::test]
async fn an_ordinary_run_records_the_process_directory() {
    let dir = TempDir::new().expect("temp dir");
    let engine = engine_with(&dir, SessionOrigin::default());
    engine
        .session_create(SessionCreateParams {
            session_id: Some("here".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");

    let expected = std::env::current_dir()
        .expect("a cwd")
        .display()
        .to_string();
    match header_of(&engine, "here") {
        KnownEvent::SessionStart { cwd, .. } => assert_eq!(cwd.as_deref(), Some(expected.as_str())),
        other => panic!("expected a header, got {other:?}"),
    }
}

/// TC-ORIGIN-3: a root session carries no delegation facts at all.
///
/// §4.4.9: "all four are optional", and TC-PROTO-30's rule is that absent
/// means absent - a reader cannot tell `"depth": null` from a session opened
/// nowhere, and there is no such thing. A root session is depth *absent*, not
/// depth zero, because zero would be a level.
///
/// Expected: neither key appears in the JSON the engine wrote.
#[tokio::test]
async fn a_root_session_carries_no_delegation_facts() {
    let dir = TempDir::new().expect("temp dir");
    let engine = engine_with(&dir, SessionOrigin::default());
    engine
        .session_create(SessionCreateParams {
            session_id: Some("root".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");

    let live = engine.sessions().live("root").expect("live");
    let data = live.log.events()[0].data.clone();
    assert!(
        data.get("spawned_by").is_none(),
        "no null was serialized: {data}"
    );
    assert!(
        data.get("depth").is_none(),
        "no null was serialized: {data}"
    );
    assert!(data.get("parent_session").is_none(), "{data}");
}

/// TC-ORIGIN-4: a delegated session records who started it and how deep it is.
///
/// §4.4.9: `depth` "is durable rather than held in memory because the bound
/// has to survive a resume: a subagent whose harness restarted must not come
/// back believing it is a root session and free to delegate again."
///
/// Expected: both facts are on the header, and both survive a reopen by a
/// second engine that was never told them.
#[tokio::test]
async fn a_delegated_session_records_who_started_it_and_how_deep() {
    let dir = TempDir::new().expect("temp dir");
    {
        let engine = engine_with(
            &dir,
            SessionOrigin {
                cwd: None,
                spawned_by: Some("parent-session".into()),
                depth: Some(2),
            },
        );
        engine
            .session_create(SessionCreateParams {
                session_id: Some("subagent".into()),
                ..SessionCreateParams::default()
            })
            .await
            .expect("create");
    }

    // A second engine, told nothing about any delegation, reopens the journal.
    let restarted = engine_with(&dir, SessionOrigin::default());
    restarted
        .session_create(SessionCreateParams {
            session_id: Some("subagent".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("reopen");

    match header_of(&restarted, "subagent") {
        KnownEvent::SessionStart {
            spawned_by, depth, ..
        } => {
            assert_eq!(spawned_by.as_deref(), Some("parent-session"));
            assert_eq!(
                depth,
                Some(2),
                "the bound survived the restart, so this session still knows it is delegated"
            );
        }
        other => panic!("expected a header, got {other:?}"),
    }
}

/// TC-ORIGIN-5: a fork inherits the origin facts it is a copy of, and its own
/// lineage is its own.
///
/// §4.4.9's last rule, and the one with two halves that pull opposite ways: a
/// fork is "the same work taken a second way and not a new piece of work", so
/// `cwd`, `spawned_by` and `depth` come across - while `parent_session` and
/// `fork_seq` name the journal it was copied from and are the child's own.
///
/// Expected: the child carries the parent's three origin facts unchanged, and
/// carries its own two lineage facts.
#[tokio::test]
async fn a_fork_inherits_the_origin_it_is_a_copy_of() {
    let dir = TempDir::new().expect("temp dir");
    let opened_in = TempDir::new().expect("temp dir");
    let engine = engine_with(
        &dir,
        SessionOrigin {
            cwd: Some(opened_in.path().to_path_buf()),
            spawned_by: Some("the-delegator".into()),
            depth: Some(1),
        },
    );
    engine
        .session_create(SessionCreateParams {
            session_id: Some("parent".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");

    engine
        .session_fork(SessionForkParams {
            session_id: "parent".into(),
            child_session_id: Some("child".into()),
            through_seq: None,
        })
        .await
        .expect("fork");

    match header_of(&engine, "child") {
        KnownEvent::SessionStart {
            cwd,
            spawned_by,
            depth,
            parent_session,
            fork_seq,
            session_id,
            ..
        } => {
            assert_eq!(session_id, "child");
            // Inherited: a fork is the same work taken a second way.
            assert_eq!(
                cwd.as_deref(),
                Some(opened_in.path().display().to_string().as_str())
            );
            assert_eq!(spawned_by.as_deref(), Some("the-delegator"));
            assert_eq!(depth, Some(1));
            // Its own: these name the journal it was copied from.
            assert_eq!(parent_session.as_deref(), Some("parent"));
            assert_eq!(fork_seq, Some(0));
        }
        other => panic!("expected a header, got {other:?}"),
    }
}

/// TC-ORIGIN-6: a fork of a root session inherits nothing it does not have.
///
/// The negative half of TC-ORIGIN-5, asserted as hard as the positive: a fork
/// must not invent a delegation for a child whose parent had none.
///
/// Expected: the child carries `cwd` and its own lineage, and neither
/// delegation key appears at all.
#[tokio::test]
async fn a_fork_of_a_root_session_invents_no_delegation() {
    let dir = TempDir::new().expect("temp dir");
    let engine = engine_with(&dir, SessionOrigin::default());
    engine
        .session_create(SessionCreateParams {
            session_id: Some("root".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    engine
        .session_fork(SessionForkParams {
            session_id: "root".into(),
            child_session_id: Some("copy".into()),
            through_seq: None,
        })
        .await
        .expect("fork");

    let live = engine.sessions().live("copy").expect("live");
    let data = live.log.events()[0].data.clone();
    assert!(data.get("spawned_by").is_none(), "{data}");
    assert!(data.get("depth").is_none(), "{data}");
    assert!(data.get("cwd").is_some(), "cwd still comes across: {data}");
    assert_eq!(data["parent_session"], serde_json::json!("root"));
}

/// TC-ORIGIN-7: a journal written before these fields existed still opens.
///
/// §4.4.9: "all four are optional, so every journal written before this parses
/// unchanged." The engine must read such a journal, keep its header as it
/// stands, and not backfill a `cwd` that would claim the session was opened
/// wherever this process happens to be.
///
/// Expected: the session reopens, the header parses, and no origin fact is
/// invented for it.
#[tokio::test]
async fn a_journal_written_before_these_fields_still_opens() {
    let dir = TempDir::new().expect("temp dir");
    std::fs::write(
        dir.path().join("old.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "type": SESSION_START,
                "seq": 0,
                "time": 1,
                "data": {
                    "session_id": "old", "provider": "mock",
                    "model": "mock-model", "max_steps": 8
                }
            })
        ),
    )
    .expect("write an old journal");

    let engine = engine_with(&dir, SessionOrigin::default());
    let info = engine
        .session_create(SessionCreateParams {
            session_id: Some("old".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("an old journal still opens");
    assert_eq!(info.session_id, "old");

    let live = engine.sessions().live("old").expect("live");
    let data = live.log.events()[0].data.clone();
    assert!(
        data.get("cwd").is_none(),
        "a reopened journal keeps the header it was written with: {data}"
    );
    assert_eq!(
        live.log.events().len(),
        1,
        "reopening wrote no second header"
    );
}

/// TC-ORIGIN-8: a fork keeps the directory the parent was opened in, not the
/// one this process is in.
///
/// The case the other fork cases cannot see. TC-ORIGIN-5 forks through the
/// same engine, so the parent's `cwd` and the process's are the same value and
/// an implementation that re-read the process directory would pass it. The
/// arrangement that tells them apart is the ordinary one: a journal opened
/// somewhere yesterday, resumed and forked from somewhere else today.
///
/// §4.4.9's rule is that a fork inherits what it is a copy of. Relabelling an
/// inherited journal with this process's directory would make the child's
/// header a claim about a run that never happened - and the field exists
/// precisely so a reader can resolve the relative paths in the history the
/// child inherited, which are the parent's.
///
/// Expected: the child's `cwd` is the parent's, and is not the forking
/// process's.
#[tokio::test]
async fn a_fork_keeps_the_parents_directory_and_not_this_ones() {
    let dir = TempDir::new().expect("temp dir");
    let opened_in = TempDir::new().expect("temp dir");
    let forked_in = TempDir::new().expect("temp dir");

    let yesterday = engine_with(
        &dir,
        SessionOrigin {
            cwd: Some(opened_in.path().to_path_buf()),
            ..SessionOrigin::default()
        },
    );
    yesterday
        .session_create(SessionCreateParams {
            session_id: Some("elsewhere".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    drop(yesterday);

    // A second process over the same journals, running somewhere else.
    let today = engine_with(
        &dir,
        SessionOrigin {
            cwd: Some(forked_in.path().to_path_buf()),
            ..SessionOrigin::default()
        },
    );
    today
        .session_fork(SessionForkParams {
            session_id: "elsewhere".into(),
            child_session_id: Some("copy".into()),
            through_seq: None,
        })
        .await
        .expect("fork");

    match header_of(&today, "copy") {
        KnownEvent::SessionStart { cwd, .. } => {
            assert_eq!(
                cwd.as_deref(),
                Some(opened_in.path().display().to_string().as_str()),
                "the child claims a directory the work it inherited never ran in"
            );
            assert_ne!(
                cwd.as_deref(),
                Some(forked_in.path().display().to_string().as_str())
            );
        }
        other => panic!("expected a header, got {other:?}"),
    }
}
