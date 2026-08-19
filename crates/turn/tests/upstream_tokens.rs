//! Test Design Specification: heuristic token pricing and the priced surface.
//!
//! Feature under test: [`tetanus_turn::tokens`] - what a request costs before a
//! provider says, and what the conversation already carries. Upstream pins the
//! same heuristic in `packages/llm/token-meter/tests/token-meter.spec.ts`
//! against `estimate.ts` and `surface-fold.ts`.
//!
//! Approach: the numbers, not a session. The estimator is a pure function, so
//! each case states the arithmetic it expects rather than a golden total: a
//! case that only compared two calls of the same function would pass with the
//! density wrong. The surface is folded from hand-written events, so a case can
//! put a non-surface event in the middle and see it ignored.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use serde_json::json;

use tetanus_session::SessionEvent;
use tetanus_turn::llm::{Message, ModelRequest};
use tetanus_turn::tokens::{
    estimate_message, estimate_request, estimate_tools, TokenSurface, BLOCK_OVERHEAD, ROLE_OVERHEAD,
};
use tetanus_turn::tools::{ToolCall, ToolSchema};

/// TC-PORT-TOKEN-1: text is priced by a fixed density, and pays framing twice.
///
/// Upstream: `token-meter.spec.ts`, "prices a text message by character
/// density with block and role overhead".
///
/// Input: a user message of exactly 16 characters.
/// Expected: 4 content tokens, plus 4 for the block, plus 4 for the role: 12.
/// The density is per character, not per word, so no case depends on language.
#[test]
fn text_is_priced_by_density_plus_block_and_role_framing() {
    let message = Message::user("0123456789abcdef");

    assert_eq!(message.content.len(), 16);
    assert_eq!(
        estimate_message(&message),
        4 + BLOCK_OVERHEAD + ROLE_OVERHEAD
    );
}

/// TC-PORT-TOKEN-2: a partial token is charged, not dropped.
///
/// Upstream: the `Math.ceil` in `estimateContent`.
///
/// Input: one character, then a full block of four, then five.
/// Expected: 1, 1 and 2 content tokens. Rounding up is what keeps the estimate
/// conservative: a budget that rounded down would promise room it lacks.
#[test]
fn a_partial_token_rounds_up() {
    let framing = BLOCK_OVERHEAD + ROLE_OVERHEAD;

    assert_eq!(estimate_message(&Message::user("a")), 1 + framing);
    assert_eq!(estimate_message(&Message::user("abcd")), 1 + framing);
    assert_eq!(estimate_message(&Message::user("abcde")), 2 + framing);
}

/// TC-PORT-TOKEN-3: a message that says nothing costs only its role.
///
/// Upstream: `token-meter.spec.ts`, "prices an empty content list at the role
/// overhead alone".
///
/// Input: an assistant message with empty text and no calls.
/// Expected: 4. Empty text is no block at all, so it does not pay block
/// framing; the role field is still on the wire.
#[test]
fn an_empty_message_costs_only_its_role_framing() {
    let message = Message::assistant("", Vec::new());

    assert_eq!(estimate_message(&message), ROLE_OVERHEAD);
}

/// TC-PORT-TOKEN-4: a tool call is priced by its name and its arguments.
///
/// Upstream: `token-meter.spec.ts`, "prices a tool-call block from name and
/// serialised arguments".
///
/// Input: an assistant message with no text and one call, `echo` with
/// `{"text":"hi"}`.
/// Expected: 1 for the four-character name, 4 for the 13-character argument
/// JSON, 4 for the block, 4 for the role: 13. The arguments are priced as they
/// go on the wire, serialised, not as the object they parse to.
#[test]
fn a_tool_call_is_priced_by_its_name_and_serialised_arguments() {
    let call = ToolCall {
        id: "c1".to_string(),
        name: "echo".to_string(),
        arguments: json!({ "text": "hi" }),
    };
    let message = Message::assistant("", vec![call.clone()]);

    assert_eq!(call.arguments.to_string().len(), 13);
    assert_eq!(
        estimate_message(&message),
        1 + 4 + BLOCK_OVERHEAD + ROLE_OVERHEAD
    );
}

/// TC-PORT-TOKEN-5: two calls in one message are priced apart.
///
/// Upstream: the per-block loop in `estimateContent`.
///
/// Input: the same call twice in one message.
/// Expected: exactly twice the one-call price, less the role framing that is
/// paid once. Framing is per block, so a message does not get cheaper by
/// carrying more.
#[test]
fn each_tool_call_pays_its_own_block_framing() {
    let call = ToolCall {
        id: "c1".to_string(),
        name: "echo".to_string(),
        arguments: json!({ "text": "hi" }),
    };
    let one = estimate_message(&Message::assistant("", vec![call.clone()]));
    let two = estimate_message(&Message::assistant("", vec![call.clone(), call]));

    assert_eq!(two, 2 * (one - ROLE_OVERHEAD) + ROLE_OVERHEAD);
}

/// TC-PORT-TOKEN-6: a tool result is a block that contains a block.
///
/// Upstream: `estimateContent`'s `tool-result` case, which prices the nested
/// content and adds its own overhead.
///
/// Input: a tool message of 16 characters, and a user message of the same text.
/// Expected: the tool message costs one more block framing than the user one.
/// The result envelope is on the wire as well as the text it reports.
#[test]
fn a_tool_result_pays_for_the_block_that_wraps_it() {
    let text = "0123456789abcdef";
    let result = estimate_message(&Message::tool("c1", text));
    let plain = estimate_message(&Message::user(text));

    assert_eq!(result, plain + BLOCK_OVERHEAD);
}

/// TC-PORT-TOKEN-7: the tool catalog is priced once, and an empty one is free.
///
/// Upstream: `estimateToolsTokens`.
///
/// Input: no tools, then one schema.
/// Expected: zero, then the serialised catalog at the fixed density plus one
/// block framing. A catalog the model is not sent costs nothing.
#[test]
fn the_tool_catalog_is_priced_once_and_an_empty_one_is_free() {
    let tools = vec![ToolSchema {
        name: "echo".to_string(),
        description: "say it back".to_string(),
        parameters: json!({ "type": "object" }),
    }];
    let json_len = serde_json::to_string(&tools).unwrap().len();

    assert_eq!(estimate_tools(&[]), 0);
    assert_eq!(
        estimate_tools(&tools),
        json_len.div_ceil(4) as u64 + BLOCK_OVERHEAD
    );
}

/// TC-PORT-TOKEN-8: a request is its catalog plus every message it carries.
///
/// Upstream: `estimateHeader` plus the surface, as `measure()` composes them.
///
/// Input: a request with a system message, a user message and one schema.
/// Expected: the sum of the three prices. The system prompt is priced as the
/// message tetanus carries it as, which is where this port differs from
/// upstream's separate request envelope.
#[test]
fn a_request_is_its_catalog_plus_its_messages() {
    let tools = vec![ToolSchema {
        name: "echo".to_string(),
        description: "say it back".to_string(),
        parameters: json!({ "type": "object" }),
    }];
    let messages = vec![Message::system("be brief"), Message::user("hello")];
    let request = ModelRequest {
        provider: "mock".to_string(),
        model: "mock-1".to_string(),
        messages: messages.clone(),
        tools: tools.clone(),
        max_tokens: None,
    };

    let parts: u64 = estimate_tools(&tools) + messages.iter().map(estimate_message).sum::<u64>();
    assert_eq!(estimate_request(&request), parts);
    assert!(estimate_request(&request) > estimate_tools(&tools));
}

/// TC-PORT-TOKEN-9: the surface holds one node per event the model sees.
///
/// Upstream: `surface-fold.ts`, "appends one node per surface event".
///
/// Input: a log of a user message, a step marker, a raw chunk, an assistant
/// message and a tool result.
/// Expected: three nodes, at the seqs of the three surface events, and a total
/// equal to their sum. A step marker and a raw chunk are on the log but not in
/// front of the model, so they are not priced.
#[test]
fn only_the_events_the_model_sees_take_a_place_on_the_surface() {
    let surface = TokenSurface::of(&[
        user(0, "hello"),
        marker(1, "step/start"),
        marker(2, "assistant/chunk"),
        assistant(3, "hi there"),
        tool_result(4, "done"),
    ]);

    let seqs: Vec<u64> = surface.nodes().iter().map(|node| node.seq).collect();
    let sum: u64 = surface.nodes().iter().map(|node| node.tokens).sum();

    assert_eq!(seqs, [0, 3, 4]);
    assert_eq!(surface.total_tokens(), sum);
    assert_eq!(
        surface.nodes()[0].tokens,
        estimate_message(&Message::user("hello"))
    );
}

/// TC-PORT-TOKEN-10: an assistant message that derives nothing still holds its
/// place, at zero.
///
/// Upstream: `foldSurfaceTokens`, "tokens is 0 when the event derives no
/// message".
///
/// Input: an assistant message with no content and no calls, between two
/// priced messages.
/// Expected: three nodes, the middle one at zero tokens. The node is what a
/// later replacement would name, so dropping it would lose the seq; pricing it
/// above zero would charge for text nobody sent.
#[test]
fn an_assistant_message_that_derives_nothing_is_a_node_at_zero() {
    let surface = TokenSurface::of(&[user(0, "hello"), assistant(1, ""), tool_result(2, "done")]);

    assert_eq!(surface.nodes().len(), 3);
    assert_eq!(surface.nodes()[1].seq, 1);
    assert_eq!(surface.nodes()[1].tokens, 0);
}

fn event(seq: u64, ty: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        ty: ty.to_string(),
        seq,
        time: 0,
        data,
        source_event_seqs: None,
    }
}

fn user(seq: u64, content: &str) -> SessionEvent {
    event(seq, "user/message", json!({ "content": content }))
}

fn assistant(seq: u64, content: &str) -> SessionEvent {
    event(seq, "assistant/message", json!({ "content": content }))
}

fn tool_result(seq: u64, content: &str) -> SessionEvent {
    event(
        seq,
        "tool/result",
        json!({ "call_id": "c1", "content": content }),
    )
}

fn marker(seq: u64, ty: &str) -> SessionEvent {
    event(seq, ty, json!({ "turn": 1, "step": 1 }))
}
