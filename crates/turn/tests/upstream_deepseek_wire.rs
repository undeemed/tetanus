//! Test Design Specification: the DeepSeek wire contract, ported.
//!
//! Feature under test: the parts of [`tetanus_turn::llm::deepseek`] that
//! upstream pins in `packages/llm/llm-deepseek/tests/serialize.spec.ts` and
//! `translate.spec.ts` and that `deepseek_adapter.rs` does not already assert:
//! what a message looks like on the wire, and what the decoder does with a
//! frame that is short of something.
//!
//! Approach: the two pure functions, `wire_request` and `StreamDecoder`, so no
//! case needs a transport, a credential or a network. Upstream's messages carry
//! a list of content blocks and tetanus carries one string plus its calls, so a
//! case ports the observable wire result rather than the block sequence.
//!
//! Features NOT tested here: thinking-mode fields, image content, and the
//! empty-response classification. tetanus carries none of them yet, and
//! `docs/parity.md` lists them as gaps rather than as passing cases.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use serde_json::json;

use tetanus_turn::llm::deepseek::{
    wire_request, StreamDecoder, DEFAULT_FINISH_REASON, EMPTY_TOOL_RESULT, PROVIDER,
};
use tetanus_turn::llm::{Message, ModelRequest, ModelResponse, StreamChunk};
use tetanus_turn::tools::{ToolCall, ToolSchema};

/// TC-PORT-DS-1: a tool that produced nothing says so.
///
/// Upstream: `serialize.spec.ts`, "sends a sentinel for empty tool-result
/// content".
///
/// Input: one tool message with empty content and one with output.
/// Expected: the empty one carries the sentinel, the other its own text
/// unchanged. Both keep the call id: the sentinel replaces what the tool said,
/// never which call it answers.
#[test]
fn a_tool_that_produced_no_output_carries_a_sentinel() {
    let wire = body(
        vec![Message::tool("c1", ""), Message::tool("c2", "ok")],
        no_tools(),
    );
    let messages = wire["messages"].as_array().expect("messages");

    assert_eq!(messages[0]["role"], "tool");
    assert_eq!(messages[0]["tool_call_id"], "c1");
    assert_eq!(messages[0]["content"], EMPTY_TOOL_RESULT);
    assert_eq!(messages[1]["content"], "ok");
}

/// TC-PORT-DS-2: an assistant turn that only calls tools still sets content.
///
/// Upstream: `serialize.spec.ts`, "serializes tool-call turns with empty string
/// content, not null" - a live-falsified shape, since the API rejects a null
/// content field.
///
/// Input: an assistant message with no text and one call.
/// Expected: `content` present and an empty string, not null and not absent.
#[test]
fn an_assistant_turn_of_calls_alone_sends_empty_content_not_null() {
    let wire = body(
        vec![Message::assistant("", vec![call("c1", "echo")])],
        no_tools(),
    );
    let message = &wire["messages"][0];

    assert_eq!(message["content"], "");
    assert!(message["content"].is_string(), "{message}");
}

/// TC-PORT-DS-3: parallel calls reach the provider in the model's order.
///
/// Upstream: `serialize.spec.ts`, "serializes parallel tool calls in order".
///
/// Input: an assistant message with two calls.
/// Expected: two wire entries in the same order, each with its own id and the
/// arguments as a JSON string. Order is what pairs a result with its call when
/// two calls name the same tool.
#[test]
fn parallel_tool_calls_keep_the_order_the_model_gave_them() {
    let calls = vec![call("c1", "read"), call("c2", "write")];
    let wire = body(vec![Message::assistant("", calls)], no_tools());
    let sent = wire["messages"][0]["tool_calls"]
        .as_array()
        .expect("tool_calls");

    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["id"], "c1");
    assert_eq!(sent[0]["function"]["name"], "read");
    assert_eq!(sent[1]["id"], "c2");
    assert_eq!(sent[1]["function"]["arguments"], r#"{"text":"hi"}"#);
}

/// TC-PORT-DS-4: no tools means no `tools` field, not an empty one.
///
/// Upstream: `serialize.spec.ts`, "omits an empty tools array".
///
/// Input: the same messages with no catalog, then with one schema.
/// Expected: the field is absent, then holds one entry. An empty array is a
/// different request: it tells the provider tools were considered and none
/// offered, which changes how some deployments bill and cache.
#[test]
fn an_empty_tool_catalog_leaves_the_field_off_the_body() {
    let empty = body(vec![Message::user("hi")], no_tools());
    let stocked = body(vec![Message::user("hi")], vec![schema()]);

    assert!(empty.get("tools").is_none(), "{empty}");
    assert_eq!(stocked["tools"].as_array().expect("tools").len(), 1);
}

/// TC-PORT-DS-5: the last usage report wins.
///
/// Upstream: `translate.spec.ts`, "last usage wins when both attached and
/// trailing arrive".
///
/// Input: usage attached to the finishing chunk, then a trailing usage-only
/// chunk with different numbers.
/// Expected: the trailing numbers. The official stream sends the authoritative
/// count last, so an earlier partial count must not stick.
#[test]
fn the_last_usage_report_is_the_one_that_counts() {
    let (_, response) = decode(&[
        r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":2}}"#,
        "[DONE]",
    ]);
    let usage = response.usage.expect("usage");

    assert_eq!(usage.prompt_tokens, 2);
    assert_eq!(usage.completion_tokens, 2);
}

/// TC-PORT-DS-6: a frame that carries no choices is not a failure.
///
/// Upstream: `translate.spec.ts`, "handles chunks with no choices at all" and
/// "omits the usage chunk when none arrived".
///
/// Input: an opening chunk with no `choices` key, a chunk with `usage: null`,
/// an empty keep-alive payload, and `[DONE]`.
/// Expected: only the one text chunk, and no usage at all. A null usage field
/// is the provider saying "not yet", not a report of zero.
#[test]
fn a_frame_without_choices_yields_nothing_and_fails_nothing() {
    let (chunks, response) = decode(&[
        r#"{"id":"x","object":"chat.completion.chunk"}"#,
        r#"{"choices":[{"delta":{"content":"hi"},"finish_reason":"stop"}],"usage":null}"#,
        "",
        "[DONE]",
    ]);

    assert_eq!(chunks, vec![text("hi")]);
    assert_eq!(response.content, "hi");
    assert!(response.usage.is_none(), "{:?}", response.usage);
}

/// TC-PORT-DS-7: a stream that never states a finish reason ended in a stop.
///
/// Upstream: `translate.spec.ts`, "defaults to finish stop when no
/// finish_reason ever arrives".
///
/// Input: one text chunk with no `finish_reason` anywhere, then `[DONE]`.
/// Expected: the reason reads `stop`. The literal is asserted beside the
/// constant so renaming the constant cannot quietly change the vocabulary the
/// session log records.
#[test]
fn a_stream_that_states_no_finish_reason_reads_as_a_stop() {
    let (_, response) = decode(&[r#"{"choices":[{"delta":{"content":"x"}}]}"#, "[DONE]"]);

    assert_eq!(response.finish_reason, DEFAULT_FINISH_REASON);
    assert_eq!(response.finish_reason, "stop");
}

/// TC-PORT-DS-8: a call survives a delta that is missing its function object.
///
/// Upstream: `translate.spec.ts`, "handles tool_call deltas with no function
/// object at all" and "with a function object but no arguments field".
///
/// Input: one delta with an id alone, one with an id and a name but no
/// arguments.
/// Expected: two calls, each with empty arguments rather than a dropped call.
/// A lenient wire is still a wire: the id is what a result is addressed to, so
/// a call that has one is worth reporting even with nothing else.
#[test]
fn a_delta_missing_its_function_object_still_yields_a_call() {
    let (chunks, response) = decode(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1"}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"c2","function":{"name":"f"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "[DONE]",
    ]);

    assert_eq!(chunks.len(), 2);
    assert_eq!(response.tool_calls, vec![bare("c1", ""), bare("c2", "f")]);
    assert_eq!(response.finish_reason, "tool_calls");
}

/// TC-PORT-DS-9: a call that never states an id is addressed by its position.
///
/// Upstream: `translate.spec.ts`, "handles deltas that never carry id or name
/// (empty-string fallbacks)". tetanus differs here on purpose: upstream reports
/// the empty string, this reports `call_{index}`.
///
/// Input: an argument fragment with neither id nor name.
/// Expected: the call is reported as `call_0`. The engine logs a result against
/// the id it was called with, so two id-less calls sharing the empty string
/// would collide; the wire index is the only thing that tells them apart.
#[test]
fn a_call_with_no_id_is_named_after_its_wire_index() {
    let frame =
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]}}]}"#;

    let (_, response) = decode(&[frame, "[DONE]"]);

    assert_eq!(response.tool_calls, vec![bare("call_0", "")]);
}

fn decode(frames: &[&str]) -> (Vec<StreamChunk>, ModelResponse) {
    let mut decoder = StreamDecoder::default();
    let mut chunks = Vec::new();
    for frame in frames {
        chunks.extend(decoder.push(frame).expect("frame decodes"));
    }
    let (tail, response) = decoder.finish();
    chunks.extend(tail);
    (chunks, response)
}

fn body(messages: Vec<Message>, tools: Vec<ToolSchema>) -> serde_json::Value {
    let request = ModelRequest {
        provider: PROVIDER.to_string(),
        model: "deepseek-v4-flash".to_string(),
        messages,
        tools,
        max_tokens: None,
    };
    wire_request(&request, None)
}

fn no_tools() -> Vec<ToolSchema> {
    Vec::new()
}

fn schema() -> ToolSchema {
    ToolSchema {
        name: "echo".to_string(),
        description: "say it back".to_string(),
        parameters: json!({ "type": "object" }),
    }
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: json!({ "text": "hi" }),
    }
}

fn bare(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: json!({}),
    }
}

fn text(delta: &str) -> StreamChunk {
    StreamChunk::Text {
        delta: delta.to_string(),
    }
}
