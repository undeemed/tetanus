//! Test Design Specification: upstream history derivation, ported.
//!
//! Feature under test: `derive_messages`, the projection of model history from
//! the durable log. Upstream pins the same rules in
//! `packages/core/session/tests/{session,surface}.spec.ts` around
//! `deriveMessages`; each case names the upstream case it comes from.
//!
//! Approach: hand-built logs, so one rule is isolated per case. The end-to-end
//! version - a real turn, replayed - is TC-PORT-LOOP-8 in `upstream_loop.rs`.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use serde_json::json;
use tetanus_session::SessionEvent;
use tetanus_turn::llm::Role;
use tetanus_turn::log::{derive_messages, topic};

/// TC-PORT-HIST-1: history is the surface events, in log order.
///
/// Upstream: `session.spec.ts`, "derives message history from the event log".
///
/// Expected: user, assistant (carrying its tool call), then the tool result
/// naming the call it answers.
#[test]
fn history_is_the_surface_events_in_log_order() {
    let log = vec![
        event(0, topic::USER_MESSAGE, json!({ "content": "echo this" })),
        event(
            1,
            topic::ASSISTANT_MESSAGE,
            json!({
                "content": "Let me echo that back.",
                "tool_calls": [{ "id": "call_1", "name": "echo", "arguments": { "text": "echo this" } }],
            }),
        ),
        event(
            2,
            topic::TOOL_RESULT,
            json!({ "call_id": "call_1", "content": "echo this", "ok": true }),
        ),
    ];

    let history = derive_messages(&log);

    let roles: Vec<Role> = history.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
    assert_eq!(history[0].content, "echo this");
    assert_eq!(history[1].tool_calls.len(), 1);
    assert_eq!(history[1].tool_calls[0].id, "call_1");
    assert_eq!(history[2].tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(history[2].content, "echo this");
}

/// TC-PORT-HIST-2: a non-surface event derives to nothing.
///
/// Upstream: `surface.spec.ts`, "surface path skips non-surface events (chunks,
/// boundaries)".
///
/// Input: a log of boundaries and raw chunks around one user message.
/// Expected: one message. The chunks stay on the log for replay and for a UI,
/// but the `assistant/message` that cites them is what history carries.
#[test]
fn boundaries_and_chunks_derive_to_nothing() {
    let log = vec![
        event(0, topic::TURN_START, json!({ "turn": 1 })),
        event(1, topic::STEP_START, json!({ "turn": 1, "step": 1 })),
        event(2, topic::USER_MESSAGE, json!({ "content": "hello" })),
        event(3, topic::ASSISTANT_CHUNK, json!({ "delta": "hel" })),
        event(4, topic::ASSISTANT_CHUNK, json!({ "delta": "lo" })),
        event(
            5,
            topic::TOOL_CALL,
            json!({ "id": "call_1", "name": "echo" }),
        ),
        event(6, topic::STEP_END, json!({ "turn": 1, "step": 1 })),
        event(7, topic::TURN_END, json!({ "turn": 1 })),
    ];

    let history = derive_messages(&log);

    assert_eq!(history.len(), 1);
    assert_eq!(history[0].role, Role::User);
    assert_eq!(history[0].content, "hello");
}

/// TC-PORT-HIST-3: an empty assistant message derives to nothing, and stays on
/// the log.
///
/// Upstream: `surface.spec.ts`, "deriveMessages skips a surface node that
/// derives to null (empty assistant/message)".
///
/// Input: an `assistant/message` with no content and no tool calls, the anchor
/// upstream appends for a step that produced neither.
/// Expected: it contributes no message, while the messages around it do, so
/// the durable record keeps usage and citations that history has no use for.
#[test]
fn an_empty_assistant_message_derives_to_nothing() {
    let log = vec![
        event(0, topic::USER_MESSAGE, json!({ "content": "hello" })),
        event(
            1,
            topic::ASSISTANT_MESSAGE,
            json!({ "content": "", "tool_calls": [], "usage": { "prompt_tokens": 7 } }),
        ),
        event(2, topic::ASSISTANT_MESSAGE, json!({ "content": "hi" })),
    ];

    let history = derive_messages(&log);

    assert_eq!(history.len(), 2, "the anchor is not history");
    assert_eq!(history[1].content, "hi");
    assert_eq!(log.len(), 3, "the anchor is still durable");
}

/// TC-PORT-HIST-4: an assistant message with tool calls but no text is history.
///
/// Upstream: `session.spec.ts`, "derives message history from the event log" -
/// a tool-only assistant turn must survive derivation, or a resumed transcript
/// loses the call its result answers.
///
/// Expected: two messages, and the tool-only one keeps its call.
#[test]
fn a_tool_only_assistant_message_is_history() {
    let log = vec![
        event(0, topic::USER_MESSAGE, json!({ "content": "run it" })),
        event(
            1,
            topic::ASSISTANT_MESSAGE,
            json!({
                "content": "",
                "tool_calls": [{ "id": "call_1", "name": "echo", "arguments": { "text": "run it" } }],
            }),
        ),
    ];

    let history = derive_messages(&log);

    assert_eq!(history.len(), 2);
    assert_eq!(history[1].role, Role::Assistant);
    assert!(history[1].content.is_empty());
    assert_eq!(history[1].tool_calls[0].name, "echo");
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
