//! Test Design Specification: upstream session-log behaviour, ported.
//!
//! Features under test: the log contract upstream deepseek-harness pins in
//! `packages/core/session/tests/{session,surface}.spec.ts`, restated against
//! the JSONL journal. Each case names the upstream case it comes from.
//!
//! Approach: a journal in a temporary directory, driven through the public
//! `SessionLog` seam only. Upstream cases that guard JavaScript hazards -
//! deep-freezing snapshots, prototype-erasing seed shells, non-serializable
//! payloads reaching an append - have no tetanus counterpart: a `SessionEvent`
//! is owned data and `data` is already a `serde_json::Value`, so the hazard
//! cannot be expressed. They are not restated as passing tests.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tetanus_core::EventBus;
use tetanus_session::{
    replay, JsonlSessionLog, SessionError, SessionEvent, SessionEventDispatch, SessionLog,
};

/// TC-PORT-SESS-1: a journal replays into exactly the log that wrote it.
///
/// Upstream: `session.spec.ts`, "replays identically from a seeded event log".
///
/// Expected: `replay(path)` equals `log.events()`, event for event, and every
/// `seq` is its position.
#[test]
fn a_journal_replays_into_the_log_that_wrote_it() {
    let (log, path, _dir) = journal("port-replay");

    log.append("turn/start", serde_json::json!({ "turn": 1 }))
        .unwrap();
    log.append_with_sources(
        "user/message",
        serde_json::json!({ "content": "hi" }),
        vec![],
    )
    .unwrap();
    log.append("turn/end", serde_json::json!({ "turn": 1 }))
        .unwrap();
    log.flush().unwrap();

    let replayed = replay(&path).unwrap();
    assert_eq!(replayed, log.events());
    for (i, event) in replayed.iter().enumerate() {
        assert_eq!(event.seq, i as u64);
    }
}

/// TC-PORT-SESS-2: a seq gap is a corrupt journal, named by line.
///
/// Upstream: `session.spec.ts`, "validates seed events: rejects a
/// non-contiguous seq".
///
/// Input: a hand-written journal whose second line claims `seq: 5`.
/// Expected: `replay` fails with `Corrupt(2)`, naming the offending line.
#[test]
fn a_seq_gap_is_a_corrupt_journal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gap.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"turn/start","seq":0,"time":1,"data":{}}"#,
            "\n",
            r#"{"type":"turn/end","seq":5,"time":2,"data":{}}"#,
            "\n",
        ),
    )
    .unwrap();

    match replay(&path) {
        Err(SessionError::Corrupt(line)) => assert_eq!(line, 2, "the offending line is named"),
        other => panic!("expected a corrupt journal, got {other:?}"),
    }
}

/// TC-PORT-SESS-3: a torn line is corrupt, and named too.
///
/// Upstream: `repair.spec.ts` covers a crash-truncated tail. tetanus does not
/// repair a journal yet (a row in `docs/parity.md` section 3); what it does
/// promise today is that the damage is reported, not read past.
///
/// Expected: `Corrupt(2)` for a half-written second line.
#[test]
fn a_torn_line_is_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("torn.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"turn/start","seq":0,"time":1,"data":{}}"#,
            "\n",
            r#"{"type":"turn/end","seq":1,"tim"#,
            "\n",
        ),
    )
    .unwrap();

    assert!(matches!(replay(&path), Err(SessionError::Corrupt(2))));
}

/// TC-PORT-SESS-4: the log is append-only through its own surface.
///
/// Upstream: `session.spec.ts`, "isolates the log from mutation through a
/// derived message (append-only contract)".
///
/// Input: clear the vector `events()` handed back, and push a forged event
/// into it.
/// Expected: the log is unchanged, and the next append continues its own
/// numbering.
#[test]
fn the_log_is_isolated_from_a_readers_mutation() {
    let (log, _path, _dir) = journal("port-append-only");
    log.append("turn/start", serde_json::json!({ "turn": 1 }))
        .unwrap();

    let mut borrowed = log.events();
    borrowed.clear();
    borrowed.push(SessionEvent {
        ty: "forged".into(),
        seq: 99,
        time: 0,
        data: serde_json::Value::Null,
        source_event_seqs: None,
    });

    let held = log.events();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].ty, "turn/start");

    let next = log
        .append("turn/end", serde_json::json!({ "turn": 1 }))
        .unwrap();
    assert_eq!(next.seq, 1, "seq is the log's, not the reader's");
}

/// TC-PORT-SESS-5: every append publishes the fact it committed.
///
/// Upstream: `session.spec.ts`, "creates sessions, emits session/created and
/// session/event". tetanus has no separate creation event - a journal's own
/// first line is its header - so only the `session/event` half ports.
///
/// Expected: one dispatch per append, in append order, each carrying the exact
/// committed event.
#[test]
fn every_append_publishes_the_committed_event() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("published.jsonl");
    let bus = EventBus::new();

    let seen: Arc<Mutex<Vec<SessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _watch = bus.on_emit::<SessionEventDispatch>(move |ev| {
        sink.lock().expect("seen").push(ev.event.clone());
    });

    let log = JsonlSessionLog::create("published", &path, bus).unwrap();
    log.append("turn/start", serde_json::json!({ "turn": 1 }))
        .unwrap();
    log.append("turn/end", serde_json::json!({ "turn": 1 }))
        .unwrap();

    assert_eq!(*seen.lock().expect("seen"), log.events());
}

/// TC-PORT-SESS-6: reopening a journal continues it, it does not restart it.
///
/// Upstream: `session.spec.ts`, "accepts a well-formed contiguous serializable
/// seed" - a seeded log keeps numbering from its seed.
///
/// Expected: the reopened log holds the earlier events, the next append is
/// `seq 2`, and the file still replays contiguously.
#[test]
fn reopening_a_journal_continues_its_numbering() {
    let (log, path, _dir) = journal("port-reopen");
    log.append("turn/start", serde_json::json!({ "turn": 1 }))
        .unwrap();
    log.append("turn/end", serde_json::json!({ "turn": 1 }))
        .unwrap();
    log.flush().unwrap();
    drop(log);

    let reopened = JsonlSessionLog::create("port-reopen", &path, EventBus::new()).unwrap();
    assert_eq!(reopened.events().len(), 2, "the seed is read back");

    let next = reopened
        .append("turn/start", serde_json::json!({ "turn": 2 }))
        .unwrap();
    assert_eq!(next.seq, 2);
    assert_eq!(replay(&path).unwrap(), reopened.events());
}

/// TC-PORT-SESS-7: a citation is recorded only where it belongs.
///
/// Upstream: `surface.spec.ts`, "a non-surface event carries no surface
/// fields" and "accepts an explicit empty source-event list on an assistant
/// message".
///
/// Expected: a plain append writes no `sourceEventSeqs` field at all; an
/// explicit empty citation survives the round trip as an empty list, which is
/// not the same as absence.
#[test]
fn only_a_surface_event_carries_its_sources() {
    let (log, path, _dir) = journal("port-sources");
    log.append("step/start", serde_json::json!({ "step": 1 }))
        .unwrap();
    log.append_with_sources(
        "assistant/message",
        serde_json::json!({ "content": "" }),
        vec![],
    )
    .unwrap();
    log.flush().unwrap();

    let replayed = replay(&path).unwrap();
    assert_eq!(replayed[0].source_event_seqs, None);
    assert_eq!(replayed[1].source_event_seqs, Some(Vec::new()));

    let written = std::fs::read_to_string(&path).unwrap();
    let first = written.lines().next().unwrap();
    assert!(
        !first.contains("sourceEventSeqs"),
        "a non-surface event writes no citation field: {first}"
    );
}

/// One journal in a temporary directory. The directory is returned so it
/// outlives the log.
fn journal(name: &str) -> (Arc<JsonlSessionLog>, std::path::PathBuf, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join(format!("{name}.jsonl"));
    let log = JsonlSessionLog::create(name, &path, EventBus::new()).expect("journal");
    (log, path, dir)
}
