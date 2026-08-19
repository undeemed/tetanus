//! Test Design Specification: crash repair, ported.
//!
//! Feature under test: `tetanus_turn::repair` - the closers an interrupted
//! journal is missing. Upstream pins the same synthesis in
//! `packages/core/session/tests/repair.spec.ts` as `interruptedTurnClosers`;
//! each case names the upstream case it comes from.
//!
//! Approach: hand-built logs, one branch per case, plus one case that commits
//! the closers to a real journal so the seq numbering is covered too.
//! Upstream's `surfaceOp` marker has no tetanus counterpart (a citation is
//! carried by `sourceEventSeqs` alone), so that half of its assertion is not
//! restated.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use serde_json::json;
use tetanus_core::EventBus;
use tetanus_session::{JsonlSessionLog, SessionEvent, SessionLog};
use tetanus_turn::log::topic;
use tetanus_turn::repair::{
    interrupted_turn_closers, repair, TOOL_NOT_STARTED, TOOL_OUTCOME_UNKNOWN,
};

/// TC-PORT-REPAIR-1: a healthy journal needs nothing.
///
/// Upstream: "returns nothing for a balanced log (ends on turn/end)" and
/// "returns nothing for an empty log".
///
/// Expected: no closers for either input, so repairing a healthy journal is a
/// no-op rather than an extra boundary.
#[test]
fn a_balanced_or_empty_log_needs_no_closers() {
    assert!(interrupted_turn_closers(&[]).is_empty());

    let balanced = vec![
        event(0, topic::TURN_START, json!({ "turn": 1 })),
        event(1, topic::TURN_END, json!({ "turn": 1, "steps": 0 })),
    ];
    assert!(interrupted_turn_closers(&balanced).is_empty());
}

/// TC-PORT-REPAIR-2: an open turn with no open step needs only its ending.
///
/// Upstream: "closes an open turn with no open step (turn/end {interrupted}
/// only)".
///
/// Expected: one `turn/end`, carrying the turn, its step count and the
/// `interrupted` reason.
#[test]
fn an_open_turn_is_closed_with_the_interrupted_reason() {
    let log = vec![event(0, topic::TURN_START, json!({ "turn": 1 }))];

    let closers = interrupted_turn_closers(&log);

    assert_eq!(types(&closers), vec![topic::TURN_END]);
    assert_eq!(closers[0].data["turn"], 1);
    assert_eq!(closers[0].data["steps"], 0);
    assert_eq!(closers[0].data["stop_reason"], "interrupted");
}

/// TC-PORT-REPAIR-3: an open step is closed before its turn.
///
/// Upstream: "closes an open step before the turn (step/end then turn/end)".
///
/// Expected: `step/end` then `turn/end`, and the turn reports the one step it
/// spent.
#[test]
fn an_open_step_is_closed_before_its_turn() {
    let log = vec![
        event(0, topic::TURN_START, json!({ "turn": 1 })),
        event(1, topic::STEP_START, json!({ "turn": 1, "step": 1 })),
    ];

    let closers = interrupted_turn_closers(&log);

    assert_eq!(types(&closers), vec![topic::STEP_END, topic::TURN_END]);
    assert_eq!(closers[0].data["step"], 1);
    assert_eq!(closers[1].data["steps"], 1);
}

/// TC-PORT-REPAIR-4: a call the crash never recorded as started is answered as
/// not started.
///
/// Upstream: "marks an assistant tool request with no recorded call as not
/// started".
///
/// Expected: `tool/result` first, then the boundaries; the result is an error
/// naming the call, coded `TOOL_NOT_STARTED`, and it cites nothing because no
/// `tool/call` was ever logged.
#[test]
fn an_unrecorded_call_is_answered_as_not_started() {
    let log = vec![
        event(0, topic::TURN_START, json!({ "turn": 2 })),
        event(1, topic::STEP_START, json!({ "turn": 2, "step": 1 })),
        event(2, topic::ASSISTANT_MESSAGE, asked("call-1")),
    ];

    let closers = interrupted_turn_closers(&log);

    assert_eq!(
        types(&closers),
        vec![topic::TOOL_RESULT, topic::STEP_END, topic::TURN_END]
    );
    let result = &closers[0];
    assert_eq!(result.data["call_id"], "call-1");
    assert_eq!(result.data["ok"], false);
    assert_eq!(result.data["code"], TOOL_NOT_STARTED);
    assert_eq!(result.sources, None);
    let text = result.data["content"].as_str().unwrap();
    assert!(text.contains("Retry it if it is still needed"), "{text}");
}

/// TC-PORT-REPAIR-5: a call that had started is answered as outcome unknown,
/// citing the call.
///
/// Upstream: "synthesized tool/result carries surfaceOp and sourceEventSeqs
/// when tool/call was logged".
///
/// Expected: the result cites the `tool/call` seq, is coded
/// `TOOL_OUTCOME_UNKNOWN`, and tells the model what is safe to retry.
#[test]
fn a_started_call_is_answered_as_outcome_unknown() {
    let log = vec![
        event(0, topic::TURN_START, json!({ "turn": 1 })),
        event(1, topic::STEP_START, json!({ "turn": 1, "step": 1 })),
        event(2, topic::ASSISTANT_MESSAGE, asked("call-1")),
        event(
            3,
            topic::TOOL_CALL,
            json!({ "id": "call-1", "name": "echo", "arguments": {} }),
        ),
    ];

    let closers = interrupted_turn_closers(&log);

    assert_eq!(
        types(&closers),
        vec![topic::TOOL_RESULT, topic::STEP_END, topic::TURN_END]
    );
    let result = &closers[0];
    assert_eq!(result.sources, Some(vec![3]));
    assert_eq!(result.data["code"], TOOL_OUTCOME_UNKNOWN);
    let text = result.data["content"].as_str().unwrap();
    assert!(
        text.contains("retry only if the operation is read-only or idempotent"),
        "{text}"
    );
    assert!(
        text.contains("first verify external state or ask the user"),
        "{text}"
    );
}

/// TC-PORT-REPAIR-6: an answered call gets no second answer.
///
/// Upstream: "does NOT synthesize a result for a tool-call that already has
/// one", and "synthesizes a result for each of multiple unanswered calls, in
/// log order".
///
/// Input: two calls asked in one message, the first already answered.
/// Expected: exactly one synthesized result, for the second call.
#[test]
fn only_an_unanswered_call_is_answered() {
    let log = vec![
        event(0, topic::TURN_START, json!({ "turn": 1 })),
        event(1, topic::STEP_START, json!({ "turn": 1, "step": 1 })),
        event(
            2,
            topic::ASSISTANT_MESSAGE,
            json!({
                "content": "",
                "tool_calls": [
                    { "id": "call-a", "name": "echo", "arguments": {} },
                    { "id": "call-b", "name": "echo", "arguments": {} },
                ],
            }),
        ),
        event(
            3,
            topic::TOOL_RESULT,
            json!({ "call_id": "call-a", "name": "echo", "ok": true, "content": "done" }),
        ),
    ];

    let closers = interrupted_turn_closers(&log);

    assert_eq!(
        types(&closers),
        vec![topic::TOOL_RESULT, topic::STEP_END, topic::TURN_END]
    );
    assert_eq!(closers[0].data["call_id"], "call-b");
}

/// TC-PORT-REPAIR-7: a closed step's calls are not repaired.
///
/// Upstream: "does NOT synthesize a result after the owning step already
/// closed".
///
/// Expected: only `turn/end`. The step committed; whatever it did with that
/// call is the driver's record, not repair's guess.
#[test]
fn a_closed_step_is_left_alone() {
    let log = vec![
        event(0, topic::TURN_START, json!({ "turn": 2 })),
        event(1, topic::STEP_START, json!({ "turn": 2, "step": 1 })),
        event(2, topic::ASSISTANT_MESSAGE, asked("call-1")),
        event(3, topic::STEP_END, json!({ "turn": 2, "step": 1 })),
    ];

    let closers = interrupted_turn_closers(&log);

    assert_eq!(types(&closers), vec![topic::TURN_END]);
}

/// TC-PORT-REPAIR-8: a committed earlier turn is not reopened.
///
/// Upstream: "synthesizes results only for the still-open turn, not a
/// committed earlier turn".
///
/// Input: turn 1 balanced with its own answered call; turn 2 crashed with an
/// unanswered one.
/// Expected: one synthesized result, for turn 2's call, and the closing
/// `turn/end` names turn 2.
#[test]
fn a_committed_earlier_turn_is_not_reopened() {
    let log = vec![
        event(0, topic::TURN_START, json!({ "turn": 1 })),
        event(1, topic::STEP_START, json!({ "turn": 1, "step": 1 })),
        event(2, topic::ASSISTANT_MESSAGE, asked("old-call")),
        event(
            3,
            topic::TOOL_RESULT,
            json!({ "call_id": "old-call", "name": "echo", "ok": true, "content": "" }),
        ),
        event(4, topic::STEP_END, json!({ "turn": 1, "step": 1 })),
        event(5, topic::TURN_END, json!({ "turn": 1, "steps": 1 })),
        event(6, topic::TURN_START, json!({ "turn": 2 })),
        event(7, topic::STEP_START, json!({ "turn": 2, "step": 1 })),
        event(8, topic::ASSISTANT_MESSAGE, asked("new-call")),
    ];

    let closers = interrupted_turn_closers(&log);

    assert_eq!(
        types(&closers),
        vec![topic::TOOL_RESULT, topic::STEP_END, topic::TURN_END]
    );
    assert_eq!(closers[0].data["call_id"], "new-call");
    assert_eq!(closers[2].data["turn"], 2);
}

/// TC-PORT-REPAIR-9: a `tool/call` nobody asked for is not answered.
///
/// Upstream: "handles tool/call without a matching assistant/message entry
/// gracefully".
///
/// Expected: the boundaries close, and no result is invented for a call the
/// assistant never requested.
#[test]
fn an_orphan_tool_call_is_not_answered() {
    let log = vec![
        event(0, topic::TURN_START, json!({ "turn": 1 })),
        event(1, topic::STEP_START, json!({ "turn": 1, "step": 1 })),
        event(
            2,
            topic::TOOL_CALL,
            json!({ "id": "orphan", "name": "echo", "arguments": {} }),
        ),
    ];

    let closers = interrupted_turn_closers(&log);

    assert_eq!(types(&closers), vec![topic::STEP_END, topic::TURN_END]);
}

/// TC-PORT-REPAIR-10: applying the closers leaves one contiguous journal.
///
/// Upstream: the persistence contract exercises the synthesis end to end; this
/// is the tetanus equivalent against the JSONL journal.
///
/// Expected: the appended closers continue the numbering, the citation
/// survives the round trip, and repairing the repaired journal appends nothing.
#[test]
fn applying_the_closers_leaves_one_contiguous_journal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("interrupted.jsonl");
    let log = JsonlSessionLog::create("interrupted", &path, EventBus::new()).unwrap();

    log.append(topic::TURN_START, json!({ "turn": 1 })).unwrap();
    log.append(topic::STEP_START, json!({ "turn": 1, "step": 1 }))
        .unwrap();
    log.append(topic::ASSISTANT_MESSAGE, asked("call-1"))
        .unwrap();
    log.append(
        topic::TOOL_CALL,
        json!({ "id": "call-1", "name": "echo", "arguments": {} }),
    )
    .unwrap();

    let written = repair(log.as_ref()).unwrap();
    log.flush().unwrap();

    assert_eq!(
        written.iter().map(|e| e.ty.as_str()).collect::<Vec<_>>(),
        vec![topic::TOOL_RESULT, topic::STEP_END, topic::TURN_END]
    );
    assert_eq!(
        written.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![4, 5, 6],
        "the closers continue the journal's numbering"
    );
    assert_eq!(written[0].source_event_seqs, Some(vec![3]));

    let replayed = tetanus_session::replay(&path).unwrap();
    assert_eq!(replayed, log.events());
    assert!(
        repair(log.as_ref()).unwrap().is_empty(),
        "a repaired journal is balanced"
    );
}

fn types(closers: &[tetanus_turn::repair::Closer]) -> Vec<&str> {
    closers.iter().map(|c| c.ty).collect()
}

/// An `assistant/message` asking for one tool call.
fn asked(id: &str) -> serde_json::Value {
    json!({
        "content": "",
        "tool_calls": [{ "id": id, "name": "echo", "arguments": {} }],
    })
}

fn event(seq: u64, ty: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        ty: ty.to_string(),
        seq,
        time: seq + 1,
        data,
        source_event_seqs: None,
    }
}
