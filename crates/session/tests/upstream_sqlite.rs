//! Test Design Specification: the SQLite session-persistence backend, ported.
//!
//! Features under test: the second backend behind the `SessionLog` seam -
//! upstream `packages/session/session-persistence-sqlite`, restated against
//! `tetanus_session::sqlite`. The claim the suite exists to hold is that a
//! caller holding a `dyn SessionLog` cannot tell the two backends apart, so
//! most cases are stated as an equality between what SQLite answers and what
//! the JSONL journal answers for the same appends.
//!
//! Approach: a database and a journal in a temporary directory, driven through
//! the public seam only. Upstream cases with no counterpart are not restated
//! as passing tests: its packed chunk rows, revision tokens, write-behind
//! coordinator and incarnation identity all serve a batching persistence layer
//! tetanus does not have, because every tetanus append is its own commit.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tetanus_core::EventBus;
use tetanus_session::sqlite::{
    export_jsonl, import_jsonl, SqliteSessionStore, APPLICATION_ID, SCHEMA_VERSION,
};
use tetanus_session::{replay, JsonlSessionLog, SessionError, SessionEvent, SessionLog};

/// The appends every backend-equality case makes, so the two backends are
/// compared over one script rather than over two hand-written ones.
fn script(log: &dyn SessionLog) {
    log.append(
        "session/start",
        serde_json::json!({ "session_id": "round" }),
    )
    .unwrap();
    log.append("turn/start", serde_json::json!({ "turn": 1 }))
        .unwrap();
    log.append_with_sources(
        "user/message",
        serde_json::json!({ "content": "count the files" }),
        vec![],
    )
    .unwrap();
    log.append_with_sources(
        "assistant/message",
        serde_json::json!({ "content": "", "tool_calls": [{"id": "c1", "name": "ls", "arguments": {}}] }),
        vec![1, 2],
    )
    .unwrap();
    log.append_with_sources(
        "tool/result",
        serde_json::json!({ "call_id": "c1", "content": "a\nb\n\u{1f600}" }),
        vec![3],
    )
    .unwrap();
    log.append("turn/end", serde_json::json!({ "turn": 1 }))
        .unwrap();
}

/// Every event but its `time`, which is a clock reading and differs between
/// two runs of the same script by construction.
fn timeless(events: &[SessionEvent]) -> Vec<SessionEvent> {
    events
        .iter()
        .cloned()
        .map(|mut event| {
            event.time = 0;
            event
        })
        .collect()
}

/// TC-PORT-STORE-Q1: a database round-trips a journal it wrote.
///
/// Upstream: `sqlite-backend.spec.ts`, the shared KV backend contract's
/// "round-trips through a reopen".
///
/// Expected: `replay` on a freshly opened store equals what the log held, and
/// every `seq` is its position.
#[test]
fn a_database_round_trips_the_journal_it_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");

    let written = {
        let store = SqliteSessionStore::open(&path).unwrap();
        let log = store.log("round", EventBus::new()).unwrap();
        script(log.as_ref());
        log.flush().unwrap();
        log.events()
    };

    let reopened = SqliteSessionStore::open(&path).unwrap();
    let read = reopened.replay("round").unwrap();
    assert_eq!(read, written);
    for (i, event) in read.iter().enumerate() {
        assert_eq!(event.seq, i as u64);
    }
}

/// TC-PORT-STORE-Q2: the two backends answer the same script identically.
///
/// This is the acceptance claim of the whole slice: a session reads the same
/// through SQLite as it does through JSONL. Only `time` differs, because the
/// two runs read the clock at different moments.
///
/// Expected: the event sequences are equal field for field once `time` is set
/// aside, and both report the same `id`.
#[test]
fn a_session_reads_identically_through_either_backend() {
    let dir = tempfile::tempdir().unwrap();

    let jsonl =
        JsonlSessionLog::create("round", dir.path().join("round.jsonl"), EventBus::new()).unwrap();
    script(jsonl.as_ref());

    let store = SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap();
    let sql = store.log("round", EventBus::new()).unwrap();
    script(sql.as_ref());

    assert_eq!(jsonl.id(), sql.id());
    assert_eq!(timeless(&jsonl.events()), timeless(&sql.events()));
    assert_eq!(
        timeless(&replay(dir.path().join("round.jsonl")).unwrap()),
        timeless(&store.replay("round").unwrap()),
    );
}

/// TC-PORT-STORE-Q3: an append is durable when it returns.
///
/// Upstream: `sqlite-backend.spec.ts`, "persists across a reopen". The tetanus
/// claim is stronger, and is the one the JSONL backend makes: no flush is
/// needed, because the append itself committed.
///
/// Expected: a second store opened on the same file, with no flush and no
/// close in between, reads every appended event.
#[test]
fn an_append_is_durable_without_a_flush() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SqliteSessionStore::open(&path).unwrap();
    let log = store.log("live", EventBus::new()).unwrap();
    script(log.as_ref());

    let observer = SqliteSessionStore::open(&path).unwrap();
    assert_eq!(observer.replay("live").unwrap(), log.events());
}

/// TC-PORT-STORE-Q4: one database holds many journals, and they do not mix.
///
/// Upstream: the `sessions`/`events` split of `schema.ts`, where the session
/// id is a column.
///
/// Expected: each id reads back only its own events, and `session_ids` reports
/// both in id order.
#[test]
fn one_database_holds_many_journals_without_mixing_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap();

    let left = store.log("alpha", EventBus::new()).unwrap();
    let right = store.log("beta", EventBus::new()).unwrap();
    left.append("turn/start", serde_json::json!({ "turn": 1 }))
        .unwrap();
    right
        .append("turn/start", serde_json::json!({ "turn": 7 }))
        .unwrap();
    right
        .append("turn/end", serde_json::json!({ "turn": 7 }))
        .unwrap();

    assert_eq!(store.session_ids().unwrap(), vec!["alpha", "beta"]);
    assert_eq!(store.replay("alpha").unwrap().len(), 1);
    assert_eq!(store.replay("beta").unwrap().len(), 2);
    assert_eq!(
        store.replay("beta").unwrap()[0].data,
        serde_json::json!({ "turn": 7 })
    );
}

/// TC-PORT-STORE-Q5: a session that has appended nothing still exists.
///
/// Upstream materializes its `sessions` row lazily, on the first append, to
/// mirror the JSONL backend's "no file until first append". tetanus's JSONL
/// backend creates the file at open, so the peer behaviour is the opposite
/// one: the row exists from the open, and the two backends agree.
///
/// Expected: `contains` is true and `replay` is empty right after `log`.
#[test]
fn an_opened_session_exists_before_its_first_append() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap();
    let _log = store.log("empty", EventBus::new()).unwrap();

    assert!(store.contains("empty").unwrap());
    assert_eq!(store.replay("empty").unwrap(), Vec::new());
    assert!(!store.contains("never-opened").unwrap());
}

/// TC-PORT-STORE-Q6: reopening a journal continues its seq numbering.
///
/// Expected: an append after a reopen carries the seq after the last stored
/// one, so the log stays contiguous across a restart.
#[test]
fn a_reopened_journal_continues_its_numbering() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    {
        let store = SqliteSessionStore::open(&path).unwrap();
        let log = store.log("resume", EventBus::new()).unwrap();
        script(log.as_ref());
    }
    let store = SqliteSessionStore::open(&path).unwrap();
    let log = store.log("resume", EventBus::new()).unwrap();
    let next = log
        .append("turn/start", serde_json::json!({ "turn": 2 }))
        .unwrap();

    assert_eq!(next.seq, 6);
    assert_eq!(store.replay("resume").unwrap().len(), 7);
}

/// TC-PORT-STORE-Q7: an unrelated database is refused, not grown a table.
///
/// Upstream: `sqlite-backend.spec.ts`, "rejects a mismatched database schema
/// version", and `schema.ts`'s ownership check on `application_id`.
///
/// Input: a database holding one unrelated table and no identity.
/// Expected: `open` fails with `ForeignStore`, and the stranger's table is
/// still the only one in the file.
#[test]
fn an_unrelated_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("someone-else.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE invoices (id TEXT PRIMARY KEY)")
        .unwrap();
    drop(connection);

    let refused = SqliteSessionStore::open(&path).unwrap_err();
    assert!(
        matches!(refused, SessionError::ForeignStore { found: 0, .. }),
        "expected ForeignStore, got {refused:?}"
    );

    let connection = rusqlite::Connection::open(&path).unwrap();
    let tables: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 1, "the refused open created no tables");
}

/// TC-PORT-STORE-Q8: a database from a future schema is refused distinctly.
///
/// Upstream: `sqlite-backend.spec.ts`, "rejects a mismatched database schema
/// version". The two refusals are distinct for the reason the storage backend
/// already gives: one is somebody else's file, the other is ours from a
/// version that may still be running, and the answers differ.
///
/// Expected: `open` fails with `ForeignSchema` naming the version it found.
#[test]
fn a_future_schema_is_refused_distinctly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(&format!(
            "PRAGMA application_id = {APPLICATION_ID};
             PRAGMA user_version = {};",
            SCHEMA_VERSION + 41
        ))
        .unwrap();
    drop(connection);

    let refused = SqliteSessionStore::open(&path).unwrap_err();
    match refused {
        SessionError::ForeignSchema { found, .. } => assert_eq!(found, SCHEMA_VERSION + 41),
        other => panic!("expected ForeignSchema, got {other:?}"),
    }
}

/// TC-PORT-STORE-Q9: a seed refuses an id the store already holds.
///
/// The seed is what a fork and an import lay down. Writing one over a journal
/// that already holds a history would splice two histories, and every seq
/// after the join would name the wrong row - which is `tetanus_session::seed`'s
/// own rule, restated on the backend that cannot express it as "the file must
/// not exist".
///
/// Expected: `Exists`, and the stored journal is untouched.
#[test]
fn a_seed_refuses_an_id_the_store_holds() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap();
    let log = store.log("taken", EventBus::new()).unwrap();
    log.append("turn/start", serde_json::json!({ "turn": 1 }))
        .unwrap();

    let refused = store
        .seed(
            "taken",
            &[SessionEvent {
                ty: "turn/start".into(),
                seq: 0,
                time: 1,
                data: serde_json::json!({ "turn": 99 }),
                source_event_seqs: None,
            }],
        )
        .unwrap_err();

    assert!(matches!(refused, SessionError::Exists(id) if id == "taken"));
    assert_eq!(store.replay("taken").unwrap().len(), 1);
}

/// TC-PORT-STORE-Q10: a seed whose seqs are not its positions is refused.
///
/// The same rule `tetanus_session::seed` enforces, so neither backend can
/// create a journal its own `replay` would refuse.
///
/// Expected: `Corrupt(2)`, naming the offending position, and no session row.
#[test]
fn a_seed_with_a_seq_gap_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap();
    let event = |seq| SessionEvent {
        ty: "turn/start".into(),
        seq,
        time: 1,
        data: serde_json::json!({}),
        source_event_seqs: None,
    };

    let refused = store.seed("gapped", &[event(0), event(5)]).unwrap_err();

    assert!(matches!(refused, SessionError::Corrupt(2)));
    assert!(!store.contains("gapped").unwrap());
}

/// TC-PORT-STORE-Q11: a JSONL journal migrates into the database unchanged.
///
/// Expected: the imported journal equals the file's, event for event -
/// including `seq`, `time` and `sourceEventSeqs`, none of which the import
/// reassigns.
#[test]
fn a_jsonl_journal_migrates_into_the_database_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("legacy.jsonl");
    let jsonl = JsonlSessionLog::create("legacy", &file, EventBus::new()).unwrap();
    script(jsonl.as_ref());
    let before = jsonl.events();
    drop(jsonl);

    let store = SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap();
    let moved = import_jsonl(&store, "legacy", &file).unwrap();

    assert_eq!(moved, before.len());
    assert_eq!(store.replay("legacy").unwrap(), before);
}

/// TC-PORT-STORE-Q12: the migration is lossless in both directions, to the byte.
///
/// The strongest statement of "either backend reads identically": a journal
/// imported and exported again is the same file, because both writers
/// serialize the same `SessionEvent`.
///
/// Expected: the exported file's bytes equal the original's, and it replays to
/// the same events.
#[test]
fn a_round_trip_through_the_database_returns_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original.jsonl");
    let jsonl = JsonlSessionLog::create("trip", &original, EventBus::new()).unwrap();
    script(jsonl.as_ref());
    drop(jsonl);

    let store = SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap();
    import_jsonl(&store, "trip", &original).unwrap();
    let back = dir.path().join("back.jsonl");
    export_jsonl(&store, "trip", &back).unwrap();

    assert_eq!(
        std::fs::read(&original).unwrap(),
        std::fs::read(&back).unwrap(),
        "an exported journal is the file that was imported, byte for byte"
    );
    assert_eq!(replay(&original).unwrap(), replay(&back).unwrap());
}

/// TC-PORT-STORE-Q13: an export refuses to overwrite an existing journal.
///
/// Expected: the export fails and the existing file keeps its content, so a
/// migration cannot silently destroy the journal it was pointed at.
#[test]
fn an_export_refuses_to_overwrite_a_journal() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap();
    let log = store.log("busy", EventBus::new()).unwrap();
    log.append("turn/start", serde_json::json!({ "turn": 1 }))
        .unwrap();

    let occupied = dir.path().join("occupied.jsonl");
    std::fs::write(&occupied, b"do not lose me\n").unwrap();

    let refused = export_jsonl(&store, "busy", &occupied).unwrap_err();

    assert!(matches!(refused, SessionError::Io(_)), "got {refused:?}");
    assert_eq!(std::fs::read(&occupied).unwrap(), b"do not lose me\n");
}

/// TC-PORT-STORE-Q14: every append is broadcast, as it is on the JSONL journal.
///
/// The bus is how a transcript, a printer or a projection hears a durable
/// fact, so a backend that stored correctly and said nothing would be
/// invisible to every observer.
///
/// Expected: one `session/event` dispatch per append, in append order.
#[test]
fn every_append_is_broadcast() {
    use std::sync::{Arc, Mutex};
    use tetanus_session::SessionEventDispatch;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteSessionStore::open(dir.path().join("sessions.db")).unwrap();
    let bus = EventBus::new();
    let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&heard);
    let _handle = bus.on_emit(move |dispatch: &SessionEventDispatch| {
        sink.lock().unwrap().push(dispatch.event.ty.clone());
    });

    let log = store.log("heard", bus).unwrap();
    script(log.as_ref());

    assert_eq!(
        *heard.lock().unwrap(),
        vec![
            "session/start",
            "turn/start",
            "user/message",
            "assistant/message",
            "tool/result",
            "turn/end",
        ]
    );
}
