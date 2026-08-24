//! Test Design Specification: reading a session journal as data.
//!
//! Feature under test: `tetanus_query` - the fold that positions a journal's
//! events, the filter language over them, the paging, and the three named
//! aggregates. This is the query half of upstream's `session-query/*`, which
//! `docs/parity.md` marks phase ③.
//!
//! Approach: every case that claims something about a real session runs a real
//! turn on the offline mock adapter through the real `HarnessEngine`, then
//! reads the journal it wrote back through the engine's own `session.events`.
//! Nothing here constructs an event by hand except the cases that are about the
//! fold's edges, where a hand-built log is the only way to produce a boundary a
//! healthy engine never writes (an unanswered call, an unclosed turn).
//!
//! Features NOT tested here: the engine's own paging of `session.events`, which
//! `crates/engine/tests` owns, and the contract's error codes, which
//! `crates/protocol/tests/wire.rs` owns. Neither is restated.
//!
//! Environmental needs: a writable temp directory. No case reaches a network or
//! an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;

use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{AgentPromptParams, Engine, SessionCreateParams, MAX_PAGE_SIZE};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::types::{SessionEvent, StopReason};
use tetanus_query::{Bound, EventFilter, Journal, Page, QueryError, Role};

/// A live engine with a journal directory of its own.
fn engine(dir: &TempDir) -> Arc<dyn Engine> {
    Arc::new(HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().join("sessions"),
        ..EngineConfig::default()
    }))
}

/// Run `turns` real mock turns and read the journal back through the engine.
///
/// The mock adapter runs the documented two-step turn - one step that calls
/// `echo`, one that answers - so a journal built this way holds a real tool
/// call and its real result, which is what these cases are about.
async fn session(dir: &TempDir, turns: usize) -> (Arc<dyn Engine>, Journal) {
    let engine = engine(dir);
    let info = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create");
    for turn in 0..turns {
        engine
            .agent_prompt(AgentPromptParams {
                session_id: info.session_id.clone(),
                content: format!("hello {turn}"),
            })
            .await
            .expect("prompt");
    }
    let journal = tetanus_query::source::load(&engine, &info.session_id)
        .await
        .expect("load");
    (engine, journal)
}

/// TC-PORT-QUERY-1: a journal read back through the engine positions every
/// event inside the turn and step it happened in.
///
/// Input: one real mock turn.
/// Expected: `session/start` is outside every turn; `turn/start` and `turn/end`
/// both belong to turn 1; the `tool/call` and its `tool/result`, which carry no
/// turn or step of their own on the wire, are placed in turn 1 step 1; and the
/// closing `assistant/message` is in turn 1 step 2.
#[tokio::test]
async fn the_fold_places_every_event_in_its_turn_and_step() {
    let dir = TempDir::new().expect("temp dir");
    let (_engine, journal) = session(&dir, 1).await;

    let at = |ty: &str| {
        journal
            .events()
            .iter()
            .find(|event| event.ty() == ty)
            .map(|event| (event.turn, event.step))
            .unwrap_or_else(|| panic!("no `{ty}` on the journal"))
    };

    assert_eq!(at("session/start"), (None, None), "before any turn");
    assert_eq!(at("turn/start"), (Some(1), None), "opens turn 1");
    assert_eq!(at("user/message"), (Some(1), Some(1)), "inside step 1");
    assert_eq!(at("tool/call"), (Some(1), Some(1)), "derived, not carried");
    assert_eq!(
        at("tool/result"),
        (Some(1), Some(1)),
        "derived, not carried"
    );
    assert_eq!(at("turn/end"), (Some(1), None), "closes turn 1");

    let last = journal
        .events()
        .iter()
        .rfind(|event| event.ty() == "assistant/message")
        .expect("a closing message");
    assert_eq!((last.turn, last.step), (Some(1), Some(2)));
}

/// TC-PORT-QUERY-2: every tool call in a session, paired with its result.
///
/// Input: two real mock turns, each of which calls `echo` once.
/// Expected: two records, in call order, each naming `echo`, each paired to a
/// result by `call_id` rather than by arrival order, each successful, and each
/// carrying the turn it was made in.
#[tokio::test]
async fn every_tool_call_in_a_session() {
    let dir = TempDir::new().expect("temp dir");
    let (_engine, journal) = session(&dir, 2).await;

    let calls = journal.tool_calls();
    assert_eq!(calls.len(), 2, "one `echo` per mock turn");
    assert!(calls.iter().all(|call| call.name == "echo"));
    assert_eq!(
        calls.iter().map(|call| call.turn).collect::<Vec<_>>(),
        vec![Some(1), Some(2)],
    );
    for call in &calls {
        assert_eq!(call.ok, Some(true), "the echo tool succeeds");
        assert!(!call.failed());
        assert!(call.result_seq.is_some(), "the log holds its answer");
        assert!(
            call.result_seq > Some(call.call_seq),
            "a result follows the call it answers",
        );
        assert!(call.output.is_some());
    }
}

/// TC-PORT-QUERY-3: a result is paired to its call by id, not by order.
///
/// Input: a hand-built log where two calls of one step are answered in the
/// reverse order - which is what parallel tool execution produces, and what an
/// ad-hoc fold zipping calls to results gets wrong.
/// Expected: each record carries the output of the result naming *its* id.
#[test]
fn pairing_is_by_call_id_under_out_of_order_results() {
    let journal = Journal::new(
        "s",
        vec![
            event(0, "turn/start", serde_json::json!({ "turn": 1 })),
            event(1, "step/start", serde_json::json!({ "turn": 1, "step": 1 })),
            event(
                2,
                "tool/call",
                serde_json::json!({ "id": "a", "name": "read", "arguments": {} }),
            ),
            event(
                3,
                "tool/call",
                serde_json::json!({ "id": "b", "name": "read", "arguments": {} }),
            ),
            event(
                4,
                "tool/result",
                serde_json::json!({ "call_id": "b", "name": "read", "ok": true, "content": "B" }),
            ),
            event(
                5,
                "tool/result",
                serde_json::json!({ "call_id": "a", "name": "read", "ok": false, "content": "A" }),
            ),
        ],
    );

    let calls = journal.tool_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].call_id, "a");
    assert_eq!(calls[0].output.as_deref(), Some("A"));
    assert_eq!(calls[0].ok, Some(false), "the second result answered it");
    assert_eq!(calls[1].call_id, "b");
    assert_eq!(calls[1].output.as_deref(), Some("B"));
    assert_eq!(calls[1].ok, Some(true));
}

/// TC-PORT-QUERY-4: every turn a given tool failed in.
///
/// Input: a hand-built log of three turns, in which `read` fails twice in turn
/// 1, succeeds in turn 2, and another tool fails in turn 3.
/// Expected: `[1]`. The turn naming two failures appears once, the turn where
/// it succeeded does not appear, and a different tool's failure is not `read`'s.
#[test]
fn every_turn_a_tool_failed_in() {
    let journal = Journal::new("s", failures());

    assert_eq!(journal.turns_failing("read"), vec![1], "grouped and unique");
    assert_eq!(journal.turns_failing("write"), vec![3]);
    assert!(
        journal.turns_failing("never-called").is_empty(),
        "a tool nobody called failed nowhere",
    );
}

/// TC-PORT-QUERY-5: an unanswered call is not a failed call.
///
/// Input: a log whose last turn was cut short after the call and before the
/// result - what a crash mid-step leaves behind.
/// Expected: the call is reported, with `ok: None`; `failed()` is false, and
/// the turn does not appear in the failure list. Nobody said the call failed,
/// so this must not say it did.
#[test]
fn an_unanswered_call_is_not_a_failure() {
    let journal = Journal::new(
        "s",
        vec![
            event(0, "turn/start", serde_json::json!({ "turn": 1 })),
            event(1, "step/start", serde_json::json!({ "turn": 1, "step": 1 })),
            event(
                2,
                "tool/call",
                serde_json::json!({ "id": "a", "name": "read", "arguments": {} }),
            ),
        ],
    );

    let calls = journal.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].ok, None, "no result said anything");
    assert!(!calls[0].failed());
    assert!(journal.turns_failing("read").is_empty());
}

/// TC-PORT-QUERY-6: the token cost of a turn range.
///
/// Input: three real mock turns, whose adapter reports usage on every
/// assistant message.
/// Expected: the cost of turns 2 through 3 counts two turns, is nonzero, is
/// strictly below the cost of all three, and reports itself complete - every
/// message in range was priced. The whole-session total is the sum of the
/// per-turn rows, so the range answer and the turn index cannot disagree.
#[tokio::test]
async fn the_token_cost_of_a_turn_range() {
    let dir = TempDir::new().expect("temp dir");
    let (_engine, journal) = session(&dir, 3).await;

    let all = journal.cost(Bound::all());
    let tail = journal.cost(Bound::span(2, 3));

    assert_eq!(all.turns, 3);
    assert_eq!(tail.turns, 2, "only the turns in range counted");
    assert!(tail.total_tokens() > 0, "the mock adapter prices its work");
    assert!(tail.total_tokens() < all.total_tokens());
    assert!(tail.complete(), "no message in range went unpriced");
    assert_eq!(tail.messages, 4, "two priced messages per mock turn");

    let rows = journal.turns();
    assert_eq!(rows.len(), 3);
    let summed: u64 = rows.iter().map(|row| row.cost.total_tokens()).sum();
    assert_eq!(
        summed,
        all.total_tokens(),
        "the index and the range agree because they read the same events",
    );
}

/// TC-PORT-QUERY-7: an unpriced message is counted as unpriced, never as free.
///
/// Input: a log with one priced and one unpriced assistant message.
/// Expected: the total is the priced one's, `unpriced` is 1, and `complete()`
/// is false. A build whose adapter reports no usage must not read as a cheap
/// session.
#[test]
fn an_unpriced_message_is_not_a_free_one() {
    let journal = Journal::new(
        "s",
        vec![
            event(0, "turn/start", serde_json::json!({ "turn": 1 })),
            event(1, "step/start", serde_json::json!({ "turn": 1, "step": 1 })),
            event(
                2,
                "assistant/message",
                serde_json::json!({
                    "content": "priced",
                    "usage": { "prompt_tokens": 10, "completion_tokens": 5 },
                }),
            ),
            event(
                3,
                "assistant/message",
                serde_json::json!({ "content": "unpriced" }),
            ),
        ],
    );

    let cost = journal.cost(Bound::all());
    assert_eq!(cost.total_tokens(), 15);
    assert_eq!(cost.messages, 1);
    assert_eq!(cost.unpriced, 1);
    assert!(!cost.complete(), "the total is a floor, and says so");
}

/// TC-PORT-QUERY-8: clauses are ANDed, values within one clause are ORed.
///
/// Input: a real one-turn journal, filtered three ways.
/// Expected: a type clause of two types matches the union of both; adding a
/// turn clause intersects; and a role clause selects by the event type's
/// domain rather than by a list of known types.
#[tokio::test]
async fn clauses_and_values_combine_the_documented_way() {
    let dir = TempDir::new().expect("temp dir");
    let (_engine, journal) = session(&dir, 2).await;

    let both = journal
        .select(&EventFilter::default().types(["tool/call", "tool/result"]))
        .expect("valid");
    assert_eq!(both.count(), 4, "two calls and two results, ORed");

    let first = journal
        .select(
            &EventFilter::default()
                .types(["tool/call", "tool/result"])
                .turns(Bound::exactly(1)),
        )
        .expect("valid");
    assert_eq!(first.count(), 2, "ANDed with the turn clause");

    let tools = journal
        .select(&EventFilter::default().roles([Role::Tool]))
        .expect("valid");
    assert_eq!(
        tools.count(),
        both.count(),
        "the `tool` domain is exactly the tool events",
    );
}

/// TC-PORT-QUERY-9: an absent clause and an empty one ask different questions.
///
/// Input: the same journal, once with no `tools` clause and once with an empty
/// one.
/// Expected: absent matches every event; empty matches none. A representation
/// that could not tell them apart would turn "the user selected no tools" into
/// "every event in the log".
#[tokio::test]
async fn an_empty_clause_matches_nothing_and_an_absent_one_matches_everything() {
    let dir = TempDir::new().expect("temp dir");
    let (_engine, journal) = session(&dir, 1).await;

    let absent = journal.select(&EventFilter::default()).expect("valid");
    assert_eq!(absent.count(), journal.len());
    assert!(absent.count() > 0);

    let empty = journal
        .select(&EventFilter::default().tools(Vec::<String>::new()))
        .expect("valid");
    assert_eq!(empty.count(), 0, "asked about no tool, answered nothing");
}

/// TC-PORT-QUERY-10: a tool clause matches only events that name a tool.
///
/// Input: `tools: ["echo"]` over a journal whose non-tool events outnumber its
/// tool events.
/// Expected: exactly the `echo` call and its result. An event with no tool is
/// not one of the tools asked for, so it does not match by default.
#[tokio::test]
async fn a_tool_clause_excludes_events_that_name_no_tool() {
    let dir = TempDir::new().expect("temp dir");
    let (_engine, journal) = session(&dir, 1).await;

    let hits = journal
        .select(&EventFilter::default().tools(["echo"]))
        .expect("valid");
    assert_eq!(hits.count(), 2);
    assert!(hits
        .iter()
        .all(|event| event.tool.as_deref() == Some("echo")));

    let other = journal
        .select(&EventFilter::default().tools(["not-a-tool"]))
        .expect("valid");
    assert_eq!(other.count(), 0);
}

/// TC-PORT-QUERY-11: the `ok` clause selects results and only results.
///
/// Input: `ok: false` and `ok: true` over the hand-built failure log.
/// Expected: `false` finds the three failing results and no call; `true` finds
/// the one succeeding result. An event with no outcome matches neither, which
/// is why `ok` is a tri-state rather than a boolean defaulted to success.
#[test]
fn the_ok_clause_matches_only_events_that_have_an_outcome() {
    let journal = Journal::new("s", failures());

    let failed = journal
        .select(&EventFilter::default().ok(false))
        .expect("valid");
    assert_eq!(failed.count(), 3);
    assert!(failed.iter().all(|event| event.ty() == "tool/result"));

    let passed = journal
        .select(&EventFilter::default().ok(true))
        .expect("valid");
    assert_eq!(passed.count(), 1);
}

/// TC-PORT-QUERY-12: filtering by time selects by the log's own clock.
///
/// Input: a hand-built log whose events are one millisecond apart, filtered to
/// an inclusive window over the middle of it.
/// Expected: exactly the events inside the window, both ends included.
#[test]
fn time_ranges_are_inclusive_at_both_ends() {
    let events: Vec<SessionEvent> = (0..5)
        .map(|seq| SessionEvent {
            ty: "user/message".into(),
            seq,
            time: 1_000 + seq,
            data: serde_json::json!({ "content": "x" }),
            source_event_seqs: None,
        })
        .collect();
    let journal = Journal::new("s", events);

    let window = journal
        .select(&EventFilter::default().time(Bound::span(1_001, 1_003)))
        .expect("valid");
    assert_eq!(
        window.iter().map(|event| event.time()).collect::<Vec<_>>(),
        vec![1_001, 1_002, 1_003],
    );
}

/// TC-PORT-QUERY-13: the text clause is a literal, case-insensitive scan of
/// what was said, and nothing else.
///
/// Input: a real journal searched for a word the user wrote, and for a regular
/// expression metacharacter sequence.
/// Expected: the user message is found regardless of case; the metacharacter
/// string finds nothing, because it is matched literally rather than compiled;
/// and no `assistant/chunk` is ever a hit, because its text arrives again whole
/// on the message that closes the step and would otherwise be counted twice.
#[tokio::test]
async fn text_is_literal_case_insensitive_and_skips_stream_chunks() {
    let dir = TempDir::new().expect("temp dir");
    let (_engine, journal) = session(&dir, 1).await;

    let found = journal
        .select(&EventFilter::default().text("HELLO 0"))
        .expect("valid");
    assert!(found.count() > 0, "case folded before comparison");
    assert!(
        found.iter().all(|event| event.ty() != "assistant/chunk"),
        "a stream chunk is never a text hit",
    );

    let literal = journal
        .select(&EventFilter::default().text("h.llo"))
        .expect("valid");
    assert_eq!(literal.count(), 0, "matched as text, not as a pattern");
}

/// TC-PORT-QUERY-14: a large selection pages by seq, and the last page says so.
///
/// Input: a selection of 12 events, read three at a time.
/// Expected: four full pages then the end; every page continues from the seq
/// the previous one handed back; no event is served twice or skipped; and only
/// the final page reports `eof`.
#[test]
fn a_selection_pages_by_seq_and_reports_its_end() {
    let events: Vec<SessionEvent> = (0..12)
        .map(|seq| SessionEvent {
            ty: "user/message".into(),
            seq,
            time: seq,
            data: serde_json::json!({ "content": "x" }),
            source_event_seqs: None,
        })
        .collect();
    let journal = Journal::new("s", events);
    let all = journal.select(&EventFilter::default()).expect("valid");
    assert_eq!(all.count(), 12, "the count is the whole, not the page");

    let mut seen = Vec::new();
    let mut page = Page::first(3);
    let mut ends = Vec::new();
    loop {
        let result = all.page(page);
        seen.extend(result.events.iter().map(|event| event.seq()));
        ends.push(result.eof);
        if result.eof {
            break;
        }
        page = Page::from(result.next_seq, 3);
    }

    assert_eq!(
        seen,
        (0..12).collect::<Vec<u64>>(),
        "each event exactly once"
    );
    assert_eq!(
        ends,
        vec![false, false, false, true],
        "only the last is the end"
    );
}

/// TC-PORT-QUERY-15: a page size of zero asks for the server maximum, and a
/// larger one is clamped to it.
///
/// Input: `limit: 0` and `limit: MAX_PAGE_SIZE + 1` over a short selection.
/// Expected: both serve the whole selection and report the end. Zero must not
/// mean "no events": a pager treating a short page as the end would stall
/// forever on one.
#[test]
fn a_zero_limit_is_the_maximum_and_an_oversized_one_is_clamped() {
    let events: Vec<SessionEvent> = (0..4)
        .map(|seq| SessionEvent {
            ty: "user/message".into(),
            seq,
            time: seq,
            data: serde_json::json!({ "content": "x" }),
            source_event_seqs: None,
        })
        .collect();
    let journal = Journal::new("s", events);
    let all = journal.select(&EventFilter::default()).expect("valid");

    for limit in [0, MAX_PAGE_SIZE + 1] {
        let page = all.page(Page::first(limit));
        assert_eq!(page.events.len(), 4, "limit {limit} served everything");
        assert!(page.eof);
    }
}

/// TC-PORT-QUERY-16: an inverted range is refused rather than answered empty.
///
/// Input: `turns: 5..=2`.
/// Expected: `InvalidFilter`, naming the clause at fault, and an `RpcError`
/// with the contract's `InvalidParams` code when it crosses a boundary. An
/// empty page would read as "this session has no such turns", which is a claim
/// about the session rather than about the ask.
#[test]
fn an_inverted_range_is_refused_and_names_its_clause() {
    let journal = Journal::new("s", Vec::new());

    let refused = journal
        .select(&EventFilter::default().turns(Bound::span(5, 2)))
        .expect_err("a range that can never match");
    let QueryError::InvalidFilter(why) = &refused else {
        panic!("expected an invalid filter, got {refused:?}");
    };
    assert!(why.contains("turns"), "names the clause: {why}");

    let wire = tetanus_protocol::rpc::RpcError::from(refused);
    assert_eq!(wire.kind(), Some(ErrorCode::InvalidParams));
}

/// TC-PORT-QUERY-17: the turn index summarises each turn from its own events.
///
/// Input: one real mock turn.
/// Expected: one row for turn 1, with the two steps the log recorded, the
/// natural stop reason it closed with, its single successful tool call, and a
/// seq span covering the whole turn.
#[tokio::test]
async fn the_turn_index_summarises_each_turn() {
    let dir = TempDir::new().expect("temp dir");
    let (_engine, journal) = session(&dir, 1).await;

    let rows = journal.turns();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.turn, 1);
    assert_eq!(row.steps, 2, "the mock turn calls a tool, then answers");
    assert_eq!(row.stop_reason, Some(StopReason::Natural));
    assert_eq!(row.tool_calls, 1);
    assert_eq!(row.tool_failures, 0);
    assert!(row.first_seq < row.last_seq);
    assert_eq!(journal.last_turn(), Some(1));
}

/// TC-PORT-QUERY-18: a turn the log never closed is reported as unclosed
/// rather than omitted.
///
/// Input: a log ending mid-turn.
/// Expected: the row exists, counts the step that did end, and carries no stop
/// reason. Dropping it would hide the exact turn a reader is looking for.
#[test]
fn an_unclosed_turn_is_still_a_row() {
    let journal = Journal::new(
        "s",
        vec![
            event(0, "turn/start", serde_json::json!({ "turn": 1 })),
            event(1, "step/start", serde_json::json!({ "turn": 1, "step": 1 })),
            event(2, "step/end", serde_json::json!({ "turn": 1, "step": 1 })),
            event(3, "step/start", serde_json::json!({ "turn": 1, "step": 2 })),
        ],
    );

    let rows = journal.turns();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].steps, 1, "one step actually ended");
    assert_eq!(rows[0].stop_reason, None, "the turn never closed");
}

/// TC-PORT-QUERY-19: loading through the engine reads the whole journal, and a
/// session that does not exist is refused with the contract's own code.
///
/// Input: a real session loaded through `session.events`, and an id nobody
/// created.
/// Expected: the load returns every event the engine will serve, and the
/// unknown id fails with `SessionNotFound` rather than an empty journal.
#[tokio::test]
async fn loading_pages_the_whole_journal_and_an_unknown_id_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let (engine, journal) = session(&dir, 1).await;

    let served = engine
        .session_events(tetanus_protocol::methods::SessionEventsParams {
            session_id: journal.session_id().to_string(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events");
    assert_eq!(journal.len(), served.events.len());
    assert!(served.eof, "one page held it all, and the load agreed");

    let missing = tetanus_query::source::load(&engine, "no-such-session")
        .await
        .expect_err("no such session");
    assert_eq!(missing.kind(), Some(ErrorCode::SessionNotFound));
}

// ---- fixtures -------------------------------------------------------------

fn event(seq: u64, ty: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        ty: ty.into(),
        seq,
        time: 1_000 + seq,
        data,
        source_event_seqs: None,
    }
}

/// Three turns: `read` fails twice in turn 1, succeeds in turn 2, and `write`
/// fails in turn 3.
fn failures() -> Vec<SessionEvent> {
    let mut events = Vec::new();
    let mut seq = 0;
    let mut push = |ty: &str, data: serde_json::Value| {
        let built = event(seq, ty, data);
        seq += 1;
        built
    };
    for (turn, calls) in [
        (1u64, vec![("read", false), ("read", false)]),
        (2, vec![("read", true)]),
        (3, vec![("write", false)]),
    ] {
        events.push(push("turn/start", serde_json::json!({ "turn": turn })));
        events.push(push(
            "step/start",
            serde_json::json!({ "turn": turn, "step": 1 }),
        ));
        for (index, (name, ok)) in calls.into_iter().enumerate() {
            let id = format!("{turn}-{index}");
            events.push(push(
                "tool/call",
                serde_json::json!({ "id": id, "name": name, "arguments": {} }),
            ));
            events.push(push(
                "tool/result",
                serde_json::json!({ "call_id": id, "name": name, "ok": ok, "content": "" }),
            ));
        }
        events.push(push(
            "step/end",
            serde_json::json!({ "turn": turn, "step": 1 }),
        ));
        events.push(push(
            "turn/end",
            serde_json::json!({ "turn": turn, "steps": 1, "stop_reason": "natural" }),
        ));
    }
    events
}
