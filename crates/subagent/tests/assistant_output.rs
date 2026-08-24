//! Conformance: what a child agent actually answered.
//!
//! Feature under test: `tetanus_subagent::assistant_output` — the rule that
//! picks a delegated run's final answer out of its journal.
//!
//! Ported from upstream
//! `packages/subagent/subagent/tests/assistant-output.spec.ts`.
//! Case ids TC-SUB-OUT-1..10. The last three are this port's own.

use serde_json::json;
use tetanus_session::SessionEvent;
use tetanus_subagent::assistant_output::{final_assistant_output, AssistantOutputFold};

fn event(ty: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        ty: ty.to_owned(),
        seq: 0,
        time: 0,
        data,
        source_event_seqs: None,
    }
}

/// An assistant message carrying `content`.
fn message(content: &str) -> SessionEvent {
    event("assistant/message", json!({ "content": content }))
}

/// One streamed text delta.
fn chunk(delta: &str) -> SessionEvent {
    event("assistant/chunk", json!({"chunk": "text", "delta": delta}))
}

/// TC-SUB-OUT-1: a child that said nothing has no answer.
#[test]
fn a_child_that_produced_nothing_has_no_answer() {
    assert_eq!(final_assistant_output(&[]), None);
    assert_eq!(
        final_assistant_output(&[event("turn/start", json!({"turn": 1}))]),
        None
    );
}

/// TC-SUB-OUT-2: one message is the answer.
#[test]
fn a_single_message_is_the_answer() {
    assert_eq!(
        final_assistant_output(&[message("done")]).as_deref(),
        Some("done")
    );
}

/// TC-SUB-OUT-3: the *last* non-empty message wins.
///
/// A child that kept working after an intermediate answer meant the later one.
#[test]
fn the_last_non_empty_message_wins() {
    let events = [message("first"), chunk("noise"), message("second")];
    assert_eq!(final_assistant_output(&events).as_deref(), Some("second"));
}

/// TC-SUB-OUT-4: an empty message does not displace a real answer.
///
/// The loop appends an empty message to record usage after a step with no
/// visible output. Letting it win would erase what the child just said.
#[test]
fn an_empty_message_does_not_erase_the_answer_before_it() {
    let events = [message("the real answer"), message("")];
    assert_eq!(
        final_assistant_output(&events).as_deref(),
        Some("the real answer")
    );
}

/// TC-SUB-OUT-5: with no message at all, the streamed text is the answer.
#[test]
fn streamed_text_is_the_fallback() {
    let events = [chunk("par"), chunk("tial")];
    assert_eq!(final_assistant_output(&events).as_deref(), Some("partial"));
}

/// TC-SUB-OUT-6: a message outranks streamed text, whichever came first.
#[test]
fn a_message_outranks_streamed_text_in_either_order() {
    assert_eq!(
        final_assistant_output(&[chunk("streamed"), message("settled")]).as_deref(),
        Some("settled")
    );
    assert_eq!(
        final_assistant_output(&[message("settled"), chunk("streamed")]).as_deref(),
        Some("settled")
    );
}

/// TC-SUB-OUT-7: records that are not the child's output contribute nothing.
#[test]
fn unrelated_records_contribute_nothing() {
    let events = [
        event("turn/start", json!({"turn": 1})),
        event("tool/call", json!({"name": "Bash"})),
        event(
            "assistant/chunk",
            json!({"chunk": "reasoning", "delta": "thinking"}),
        ),
        event("tool/result", json!({"content": "output"})),
        event("turn/end", json!({"turn": 1})),
    ];
    assert_eq!(
        final_assistant_output(&events),
        None,
        "reasoning is not the answer, and neither is a tool result"
    );
}

/// TC-SUB-OUT-8: the fold can be read while it is still growing.
///
/// This port's own. A backend watching a live child asks repeatedly, and a
/// `collect` that consumed the fold or reset the fallback would give a
/// different answer the second time it was asked.
#[test]
fn the_fold_can_be_read_repeatedly_while_it_grows() {
    let mut fold = AssistantOutputFold::new();
    assert_eq!(fold.collect(), None);

    fold.push(&chunk("half "));
    assert_eq!(fold.collect().as_deref(), Some("half "));
    assert_eq!(
        fold.collect().as_deref(),
        Some("half "),
        "asking twice gives the same answer"
    );

    fold.push(&chunk("way"));
    assert_eq!(fold.collect().as_deref(), Some("half way"));

    fold.push(&message("final"));
    assert_eq!(fold.collect().as_deref(), Some("final"));
    assert_eq!(fold.collect().as_deref(), Some("final"));
}

/// TC-SUB-OUT-9: text arriving outside the journal reaches the same fallback.
///
/// This port's own. A transport that carries content without journal records
/// has no `assistant/chunk` to push, and if `push_text` fed a different buffer
/// the two sources would not concatenate into one answer.
#[test]
fn text_from_outside_the_journal_joins_the_same_fallback() {
    let mut fold = AssistantOutputFold::new();
    fold.push(&chunk("from the journal "));
    fold.push_text("and from the transport");
    assert_eq!(
        fold.collect().as_deref(),
        Some("from the journal and from the transport")
    );

    // An empty piece is not a piece, so it cannot turn "no answer" into "".
    let mut empty = AssistantOutputFold::new();
    empty.push_text("");
    assert_eq!(empty.collect(), None);
}

/// TC-SUB-OUT-10: a malformed record is ignored, not fatal.
///
/// This port's own. These records are read back off disk, where a truncated
/// write or an older writer can leave a field missing or wrong-typed. A fold
/// that panicked would fail the parent's run over a child's journal, which is
/// the opposite of what reading a result is for.
#[test]
fn a_malformed_record_is_ignored_rather_than_fatal() {
    let events = [
        event("assistant/message", json!({})),
        event("assistant/message", json!({"content": 42})),
        event("assistant/message", json!({"content": null})),
        event("assistant/chunk", json!({"chunk": "text"})),
        event("assistant/chunk", json!({"delta": "no tag"})),
        event("assistant/chunk", json!("not even an object")),
        message("survived"),
    ];
    assert_eq!(final_assistant_output(&events).as_deref(), Some("survived"));
}
