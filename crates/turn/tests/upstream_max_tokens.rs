//! Test Design Specification: a turn the provider's output cap cut off,
//! ported.
//!
//! Feature under test: what `TurnEngine::run_turn` does with a completion that
//! stopped because the model ran out of room rather than because it had
//! finished - the reason the turn ends with, the calls such a step does not
//! dispatch, and the words a provider spells that ending with. Upstream pins
//! the same rules in `packages/core/agent-loop/tests/loop.spec.ts`; each case
//! names the upstream case it comes from.
//!
//! Approach: the offline fixture the other turn suites use, with the provider
//! replaced by an `llm/stream` listener answering a scripted queue of
//! responses, and a tool that counts the times it ran. A cut-off completion is
//! then just a response whose `finish_reason` is the provider's word for the
//! cap, which is what the adapter would have decoded off a real stream.
//!
//! Features NOT tested here: how a truncated turn crosses the wire
//! (`crates/engine/tests/max_tokens.rs`), and which finish reason a live
//! DeepSeek stream decodes (`deepseek_adapter.rs`). Upstream's sticky case -
//! a cut-off step earlier in a turn that is steered into a later step which
//! finishes normally - has nothing to restate: it needs a listener that can
//! continue a turn past the cut, and phase ① has no inbox to steer with, so a
//! cut-off step always ends the turn it is on. `docs/parity.md` carries the
//! gap.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key, and no case waits on a clock.
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
use tetanus_turn::llm::{ModelResponse, TRUNCATED_FINISH_REASONS};
use tetanus_turn::log::{derive_messages, topic};
use tetanus_turn::tools::{Tool, ToolCall, ToolError, ToolOutcome, ToolRegistry, ToolSchema};
use tetanus_turn::StopReason;

/// TC-PORT-CAP-1: a last step the cap cut off ends the turn `max-tokens`.
///
/// Upstream: `loop.spec.ts`, "surfaces max-tokens as the turn-end reason when
/// the last step is cut off".
///
/// Input: one response carrying half an answer and `finish_reason: "length"`,
/// with no tool calls.
/// Expected: the provider was asked once, the outcome reads `MaxTokens`, and
/// the durable `turn/end` says `max-tokens` for one step. The reason is
/// asserted on the journal as well as on the returned outcome, because a
/// surface reading the file is the one that has to say the answer is
/// unfinished.
#[tokio::test]
async fn a_last_step_the_cap_cut_off_ends_the_turn_at_the_cap() {
    let h = Harness::new("cap-last-step").await;
    let (asked, _provider) = answers(h.bus(), vec![cut_off("truncat", vec![])]);

    let outcome = h.engine.run_turn("go").await.expect("the turn ran");

    assert_eq!(asked.load(Ordering::Relaxed), 1);
    assert_eq!(outcome.reason, StopReason::MaxTokens);
    assert_eq!(outcome.steps, 1);
    let journal = journal(&h);
    let end = last(&journal, topic::TURN_END);
    assert_eq!(end.data["stop_reason"], "max-tokens");
    assert_eq!(end.data["steps"], 1);
}

/// TC-PORT-CAP-2: the cap does not leak into the next turn.
///
/// Upstream: `loop.spec.ts`, "a completed step after no max-tokens keeps the
/// turn completed (max-tokens does not leak across turns)".
///
/// Input: two prompts on one engine - the first answered by a cut-off
/// response, the second by a finished one.
/// Expected: the journal carries `max-tokens` then `natural`, in that order.
/// The reason is the turn's, and the engine keeps one turn's state across
/// prompts, so this is what says the cut-off turn did not colour the next.
#[tokio::test]
async fn the_cap_does_not_colour_the_turn_after_it() {
    let h = Harness::new("cap-no-leak").await;
    let (_asked, _provider) = answers(
        h.bus(),
        vec![cut_off("cut", vec![]), finished("clean", vec![])],
    );

    let first = h.engine.run_turn("first").await.expect("turn 1 ran");
    let second = h.engine.run_turn("second").await.expect("turn 2 ran");

    assert_eq!(first.reason, StopReason::MaxTokens);
    assert_eq!(second.reason, StopReason::Natural);
    assert_eq!(reasons(&journal(&h)), ["max-tokens", "natural"]);
}

/// TC-PORT-CAP-3: a step the cap cut off dispatches none of the calls it
/// carries.
///
/// Upstream: `loop.spec.ts`, "does not dispatch tool calls from a
/// max-tokens-truncated step".
///
/// Input: one cut-off response carrying a well-formed call on a tool that
/// counts its runs.
/// Expected: the tool never ran, the journal holds no `tool/call` and no
/// `tool/result`, and the turn ends `max-tokens` after one step. A completion
/// that stopped mid-write can have stopped in the middle of a call's
/// arguments, so arguments that happen to parse are not evidence the model
/// finished writing them.
#[tokio::test]
async fn a_step_the_cap_cut_off_dispatches_nothing() {
    let ran = Arc::new(AtomicUsize::new(0));
    let h = Harness::with_tools(
        "cap-no-dispatch",
        ToolRegistry::new().with(Arc::new(Counted(Arc::clone(&ran)))),
    )
    .await;
    let (_asked, _provider) = answers(h.bus(), vec![cut_off("", vec![call("c1")])]);

    let outcome = h.engine.run_turn("go").await.expect("the turn ran");

    assert_eq!(ran.load(Ordering::Relaxed), 0, "the counted tool ran");
    let journal = journal(&h);
    assert_eq!(
        count(&journal, topic::TOOL_CALL),
        0,
        "a call was dispatched"
    );
    assert_eq!(count(&journal, topic::TOOL_RESULT), 0, "a result was owed");
    assert_eq!(outcome.reason, StopReason::MaxTokens);
    assert_eq!(outcome.steps, 1, "no step followed the cut-off one");
}

/// TC-PORT-CAP-4: the cut-off step still leaves its own anchor and its own
/// closers.
///
/// Upstream: `loop.spec.ts`, "appends an empty completion anchor for a
/// max-tokens step with no usage".
///
/// Input: a cut-off response with no content and one call, then the journal
/// read back off disk.
/// Expected: one `assistant/message`, recording the cut-off finish reason and
/// carrying none of the calls the step refused to make; the journal ends
/// `step/end`, `turn/end`; and the derived history is the user's prompt alone,
/// because a silent assistant message is not one the model reads back.
///
/// It found the defect the anchor is written around: the step already
/// dispatched nothing, but the call stayed on the anchor, so the derived
/// history carried an assistant message asking for a call that no `tool`
/// message would ever answer - which is the shape an OpenAI-compatible
/// provider refuses the next request for.
#[tokio::test]
async fn the_cut_off_step_leaves_an_anchor_and_no_dangling_call() {
    let h = Harness::new("cap-anchor").await;
    let (_asked, _provider) = answers(h.bus(), vec![cut_off("", vec![call("c1")])]);

    h.engine.run_turn("go").await.expect("the turn ran");

    let journal = journal(&h);
    assert_eq!(count(&journal, topic::ASSISTANT_MESSAGE), 1);
    let anchor = last(&journal, topic::ASSISTANT_MESSAGE);
    assert_eq!(anchor.data["finish_reason"], "length");
    assert_eq!(
        anchor.data["tool_calls"],
        json!([]),
        "the anchor kept a call no result will answer"
    );
    let types: Vec<&str> = journal.iter().map(|e| e.ty.as_str()).collect();
    assert_eq!(
        &types[types.len() - 2..],
        [topic::STEP_END, topic::TURN_END],
        "the cut-off step closes, then the turn: {types:?}"
    );
    let history = derive_messages(&journal);
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0].content, "go");
}

/// TC-PORT-CAP-5: every word a provider spells the cap with reads as cut off.
///
/// Upstream: `loop.spec.ts` reaches the same classification through its own
/// `maxTokensResponse` fixture; the words are the ones the OpenAI-compatible
/// wire uses for it.
///
/// Input: each word the OpenAI-compatible wire spells the cap with, written
/// out here rather than read from the constant, then the reasons a finished or
/// a stopped completion carries.
/// Expected: every one of those three words reads truncated, the constant
/// holds exactly them, and nothing else reads truncated. The list is what the
/// turn engine branches on, so a word missing from it is a turn that ends
/// `natural` on half an answer.
#[test]
fn the_cap_is_read_from_the_words_a_provider_sends_for_it() {
    let wire = ["length", "max_tokens", "max-tokens"];
    assert_eq!(TRUNCATED_FINISH_REASONS, wire);

    for reason in wire {
        let response = ModelResponse {
            finish_reason: reason.to_string(),
            ..ModelResponse::default()
        };
        assert!(response.truncated(), "`{reason}` reads as finished");
    }
    for reason in ["stop", "tool_calls", "content_filter", ""] {
        let response = ModelResponse {
            finish_reason: reason.to_string(),
            ..ModelResponse::default()
        };
        assert!(!response.truncated(), "`{reason}` reads as cut off");
    }
}

/// A response the provider cut off at its output cap: `length` is the word the
/// OpenAI-compatible wire DeepSeek serves uses for it.
fn cut_off(content: &str, tool_calls: Vec<ToolCall>) -> ModelResponse {
    ModelResponse {
        content: content.to_string(),
        tool_calls,
        finish_reason: "length".into(),
        ..ModelResponse::default()
    }
}

/// A response the model finished writing.
fn finished(content: &str, tool_calls: Vec<ToolCall>) -> ModelResponse {
    ModelResponse {
        content: content.to_string(),
        tool_calls,
        finish_reason: "stop".into(),
        ..ModelResponse::default()
    }
}

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: COUNTED.to_string(),
        arguments: json!({ "text": "x" }),
    }
}

/// A route answering a scripted queue, and the count of requests it was asked
/// to make. A request past the end of the queue is answered by a finished
/// response with no calls, so a case that runs one step too many ends the turn
/// instead of hanging it.
fn answers(bus: &EventBus, script: Vec<ModelResponse>) -> (Arc<AtomicUsize>, EffectHandle) {
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    let queue = Arc::new(Mutex::new(script.into_iter()));
    let handle = bus.on_waterfall::<LlmStream, _>(move |_ev, _next| {
        let next = queue.lock().expect("script").next();
        counted.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move { Ok(next.unwrap_or_else(|| finished("done", vec![]))) })
    });
    (asked, handle)
}

const COUNTED: &str = "counted";

/// A tool that records every run, so a case can say a call was not dispatched
/// rather than only that no record of it reached the journal.
struct Counted(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Tool for Counted {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: COUNTED.into(),
            description: "Count the times it was run.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
            }),
        }
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(ToolOutcome::ok("should not run"))
    }
}

/// The journal read back off disk rather than out of the log's memory: what a
/// surface or a resumed session sees is the file.
fn journal(h: &Harness) -> Vec<SessionEvent> {
    tetanus_session::replay(&h.log_path).expect("the journal reads back")
}

fn last<'a>(journal: &'a [SessionEvent], ty: &str) -> &'a SessionEvent {
    journal
        .iter()
        .rev()
        .find(|event| event.ty == ty)
        .unwrap_or_else(|| panic!("no `{ty}` on the journal"))
}

fn count(journal: &[SessionEvent], ty: &str) -> usize {
    journal.iter().filter(|event| event.ty == ty).count()
}

fn reasons(journal: &[SessionEvent]) -> Vec<String> {
    journal
        .iter()
        .filter(|event| event.ty == topic::TURN_END)
        .map(|event| event.data["stop_reason"].as_str().unwrap_or("").to_string())
        .collect()
}
