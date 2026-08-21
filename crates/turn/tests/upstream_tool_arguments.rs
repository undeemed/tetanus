//! Test Design Specification: a tool call whose arguments are not an object.
//!
//! Features under test: upstream
//! `packages/core/agent-loop/tests/coverage-edges.spec.ts`, its two
//! `tool JSON parse` cases - what the harness does when the model writes
//! arguments that are not JSON, and when it writes none at all. A model is
//! free to emit either, so both are ordinary input and neither may end a turn.
//!
//! Approach: two levels, because the decision is made in two places. The
//! decoder cases drive `StreamDecoder` over the frames a provider would send.
//! The loop cases run a whole turn against a scripted provider and read the
//! journal a surface would read, which is where upstream asserts.
//!
//! Features NOT tested here: the rest of the wire decoder
//! (`upstream_deepseek_wire.rs`), and what a tool does with the arguments it
//! is handed. tetanus validates no arguments against a schema - upstream's
//! validator and its schema DSL are the phase ② tool pipeline - so there is no
//! refusal to assert, and a tool reads what arrived.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

// This suite drives the fixture's engine with its own provider and tools; a
// test binary lints the parts of a shared fixture it does not reach for.
#[allow(dead_code)]
mod harness;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use harness::Harness;
use serde_json::json;
use tetanus_core::{EffectHandle, EventBus};
use tetanus_session::SessionEvent;
use tetanus_turn::events::LlmStream;
use tetanus_turn::llm::deepseek::StreamDecoder;
use tetanus_turn::llm::ModelResponse;
use tetanus_turn::log::topic;
use tetanus_turn::tools::{Tool, ToolCall, ToolError, ToolOutcome, ToolRegistry, ToolSchema};
use tetanus_turn::StopReason;

/// TC-PORT-ARGS-1: arguments that are not JSON arrive as the text the model
/// wrote.
///
/// Upstream: "passes through non-JSON arguments string without crashing".
///
/// Input: one tool-call delta whose `arguments` is `not json`.
/// Expected: one call, carrying that text as a JSON string. Not an error and
/// not a dropped call: what the model asked for is unusable, but which tool it
/// asked for is not, and a tool that reads raw text is entitled to it.
#[test]
fn arguments_that_are_not_json_arrive_as_text() {
    let (_, response) = decode(&[
        &arguments("c1", "shout", "not json"),
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "[DONE]",
    ]);

    assert_eq!(
        response.tool_calls,
        vec![ToolCall {
            id: "c1".to_string(),
            name: "shout".to_string(),
            arguments: json!("not json"),
        }]
    );
}

/// TC-PORT-ARGS-2: arguments the model never wrote are no arguments.
///
/// Upstream: "uses empty object when tool-call arguments are empty string".
///
/// Input: one tool-call delta whose `arguments` is the empty string, then one
/// that is nothing but whitespace.
/// Expected: both decode to `{}`. A tool that takes no parameters is the
/// ordinary reason a model writes nothing, so an empty string is the absence
/// of arguments rather than a malformed value, and it must not read as the
/// JSON string `""`.
#[test]
fn arguments_the_model_left_empty_are_an_empty_object() {
    for written in ["", "   \n"] {
        let (_, response) = decode(&[
            &arguments("c1", "noarg", written),
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);

        assert_eq!(
            response.tool_calls,
            vec![ToolCall {
                id: "c1".to_string(),
                name: "noarg".to_string(),
                arguments: json!({}),
            }],
            "arguments written as {written:?}"
        );
    }
}

/// TC-PORT-ARGS-3: a call with unusable arguments still runs, and the journal
/// records what the model actually wrote.
///
/// Upstream: the same case, asserting `tool/call.data.arguments` is the raw
/// string and that a `tool/result` was produced.
/// Input: a scripted response carrying a call whose arguments are the text
/// `not json`, on a tool that reports what it was handed.
/// Expected: the tool ran once, `tool/call` carries that text verbatim,
/// `tool/result` carries it back, and the turn closes `natural`. Recording the
/// arguments the model wrote, rather than a repaired version of them, is what
/// lets a reader see why a tool answered the way it did.
#[tokio::test]
async fn a_call_with_unusable_arguments_still_reaches_its_tool() {
    let ran = Arc::new(AtomicUsize::new(0));
    let h = Harness::with_tools(
        "args-not-json",
        ToolRegistry::new().with(Arc::new(Echo(Arc::clone(&ran)))),
    )
    .await;
    let (_asked, _provider) = answers(
        h.bus(),
        vec![
            calling(vec![call("c1", "echo", json!("not json"))]),
            finished("done"),
        ],
    );

    let outcome = h.engine.run_turn("use the tool").await.expect("turn ran");

    assert_eq!(ran.load(Ordering::Relaxed), 1);
    assert_eq!(outcome.reason, StopReason::Natural);
    let journal = journal(&h);
    assert_eq!(
        one(&journal, topic::TOOL_CALL).data["arguments"],
        "not json"
    );
    let result = one(&journal, topic::TOOL_RESULT);
    assert_eq!(result.data["ok"], true);
    assert_eq!(result.data["content"], "handed: \"not json\"");
}

/// TC-PORT-ARGS-4: a call with no arguments runs a tool that wants none.
///
/// Upstream: the same case, asserting only that a `tool/result` appeared.
/// Input: a scripted response carrying a call whose arguments are `{}`.
/// Expected: the tool ran once, `tool/call` carries `{}` and not the empty
/// string, and `tool/result` says so. The empty object is what a tool with no
/// parameters is handed, so this is the ordinary path rather than an edge.
#[tokio::test]
async fn a_call_with_no_arguments_runs_the_tool_that_wants_none() {
    let ran = Arc::new(AtomicUsize::new(0));
    let h = Harness::with_tools(
        "args-empty",
        ToolRegistry::new().with(Arc::new(Echo(Arc::clone(&ran)))),
    )
    .await;
    let (_asked, _provider) = answers(
        h.bus(),
        vec![
            calling(vec![call("c1", "echo", json!({}))]),
            finished("done"),
        ],
    );

    h.engine.run_turn("use the tool").await.expect("turn ran");

    assert_eq!(ran.load(Ordering::Relaxed), 1);
    assert_eq!(
        one(&journal(&h), topic::TOOL_CALL).data["arguments"],
        json!({})
    );
    assert_eq!(
        one(&journal(&h), topic::TOOL_RESULT).data["content"],
        "handed: {}"
    );
}

/// A tool that reports what it was handed, whatever that was. It never refuses
/// its arguments, because tetanus validates none: the point of these cases is
/// that the value reaches a tool unchanged.
struct Echo(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Tool for Echo {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "echo".to_string(),
            description: "say back what you were handed".to_string(),
            parameters: json!({ "type": "object" }),
        }
    }
    async fn execute(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(ToolOutcome::ok(format!("handed: {arguments}")))
    }
}

fn decode(frames: &[&str]) -> (Vec<tetanus_turn::llm::StreamChunk>, ModelResponse) {
    let mut decoder = StreamDecoder::default();
    let mut chunks = Vec::new();
    for frame in frames {
        chunks.extend(decoder.push(frame).expect("frame decodes"));
    }
    let (tail, response) = decoder.finish();
    chunks.extend(tail);
    (chunks, response)
}

/// One tool-call delta carrying arguments exactly as a provider would write
/// them, escaped into the frame rather than assembled by hand.
fn arguments(id: &str, name: &str, written: &str) -> String {
    json!({
        "choices": [{ "delta": { "tool_calls": [{
            "index": 0,
            "id": id,
            "function": { "name": name, "arguments": written },
        }] } }]
    })
    .to_string()
}

fn call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
    }
}

fn calling(tool_calls: Vec<ToolCall>) -> ModelResponse {
    ModelResponse {
        content: String::new(),
        tool_calls,
        finish_reason: "tool_calls".to_string(),
        ..ModelResponse::default()
    }
}

fn finished(content: &str) -> ModelResponse {
    ModelResponse {
        content: content.to_string(),
        finish_reason: "stop".to_string(),
        ..ModelResponse::default()
    }
}

/// A provider that answers from a script, so a case fixes what the model said.
fn answers(bus: &EventBus, script: Vec<ModelResponse>) -> (Arc<AtomicUsize>, EffectHandle) {
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    let queue = Arc::new(Mutex::new(script.into_iter()));
    let handle = bus.on_waterfall::<LlmStream, _>(move |_ev, _next| {
        let next = queue.lock().expect("script").next();
        counted.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move { Ok(next.unwrap_or_else(|| finished("done"))) })
    });
    (asked, handle)
}

fn journal(h: &Harness) -> Vec<SessionEvent> {
    tetanus_session::replay(&h.log_path).expect("the journal reads back")
}

/// The one event of a type, so a case that meant to see exactly one says so.
fn one(journal: &[SessionEvent], ty: &str) -> SessionEvent {
    let found: Vec<&SessionEvent> = journal.iter().filter(|event| event.ty == ty).collect();
    assert_eq!(found.len(), 1, "expected one {ty}");
    found[0].clone()
}
