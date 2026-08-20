//! Test Design Specification: upstream agent-loop behaviour, ported.
//!
//! Features under test: the loop behaviour upstream deepseek-harness pins in
//! `packages/core/agent-loop/tests/{loop,tool-order,interception}.spec.ts`,
//! restated against the surfaces tetanus serves today. Each case names the
//! upstream case it comes from.
//!
//! Approach: the same offline fixture the turn-flow suite uses. Where upstream
//! asserts on its own seam names, the case asserts on the tetanus seam that
//! carries the same decision, and the translation is stated in the case. A
//! behaviour with no tetanus surface yet is not restated here; it stays a row
//! in `docs/parity.md`.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use harness::Harness;
use tetanus_core::{EffectHandle, EventBus};
use tetanus_turn::events::{
    AgentRequest, AssemblePrompt, LlmStream, PreStep, PreStepDecision, StopReason, SystemPrompt,
    ToolsExecute,
};
use tetanus_turn::llm::{LlmError, Message, ModelRequest, Role};
use tetanus_turn::log::{derive_messages, topic};
use tetanus_turn::tools::{
    EchoTool, Tool, ToolError, ToolOrder, ToolOrderError, ToolOutcome, ToolRegistry, ToolSchema,
    TOOL_ORDER_REST,
};
use tetanus_turn::{TurnConfig, TurnError};

/// TC-PORT-LOOP-1: a tool result reaches the next model request.
///
/// Upstream: `loop.spec.ts`, "round-trips tool calls: model requests tool ->
/// executes -> result in next request".
///
/// Input: one mock turn, with every `agent/request` recorded.
/// Expected: two requests; the first carries no `tool` message; the second
/// carries exactly one, holding the echoed text and citing the call it answers.
#[tokio::test]
async fn a_tool_result_reaches_the_next_request() {
    let h = Harness::new("port-round-trip").await;
    let (requests, _record) = record_requests(h.bus());

    let outcome = h.engine.run_turn("round trip").await.unwrap();

    let requests = requests.lock().expect("requests").clone();
    assert_eq!(requests.len(), 2, "one request per step");
    assert!(
        !requests[0].messages.iter().any(|m| m.role == Role::Tool),
        "the first request is asked before any tool has run"
    );

    let results: Vec<&Message> = requests[1]
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, "round trip", "echo returns what it got");
    assert_eq!(
        results[0].tool_call_id.as_deref(),
        Some("call_1"),
        "a result names the call it answers"
    );
    assert_eq!(outcome.content, "You said: round trip");
}

/// TC-PORT-LOOP-2: `agent/pre-step` fires once per proposed step, before the
/// step opens, and reports that step's coordinates.
///
/// Upstream: `loop.spec.ts`, "agent/pre-step fires once per proposed step
/// before the step is opened"; `interception.spec.ts`, "reports the request
/// coordinates for initial and tool-continuation prompts".
///
/// Expected: two dispatches, `(turn 1, step 1)` claiming the one queued
/// message and `(turn 1, step 2)` claiming none, and the first `agent/pre-step`
/// precedes the first `step/start` in the trace.
#[tokio::test]
async fn pre_step_fires_once_per_proposed_step_with_its_coordinates() {
    let h = Harness::new("port-pre-step").await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let claims = Arc::clone(&seen);
    let _watch = h.bus().on_waterfall::<PreStep, _>(move |ev, next| {
        claims
            .lock()
            .expect("claims")
            .push((ev.turn, ev.step, ev.messages.len()));
        Box::pin(next.run(ev))
    });

    h.engine.run_turn("coordinates").await.unwrap();

    assert_eq!(
        *seen.lock().expect("claims"),
        vec![(1, 1, 1), (1, 2, 0)],
        "the continuation claim carries no new user message"
    );
    let trace = h.trace();
    let first_claim = position(&trace, "agent/pre-step");
    let first_step = position(&trace, "step/start");
    assert!(first_claim < first_step, "the claim precedes its step");
}

/// TC-PORT-LOOP-3: a rewriting `agent/pre-step` listener changes what is
/// recorded, not just what is sent.
///
/// Upstream: `interception.spec.ts`, "enter with content rewrites the prompt
/// before it is recorded".
///
/// Input: a listener that replaces the entered messages with a redacted one.
/// Expected: the journal's `user/message` holds the rewrite; the first request
/// holds the rewrite; the original text appears nowhere in the journal.
#[tokio::test]
async fn a_rewritten_claim_is_what_gets_recorded() {
    let h = Harness::new("port-rewrite").await;
    let (requests, _record) = record_requests(h.bus());

    let _rewrite = h.bus().on_waterfall::<PreStep, _>(|ev, next| {
        Box::pin(async move {
            match next.run(ev).await {
                PreStepDecision::Enter(messages) => PreStepDecision::Enter(
                    messages
                        .into_iter()
                        .map(|_| Message::user("[redacted]"))
                        .collect(),
                ),
                reject => reject,
            }
        })
    });

    h.engine.run_turn("my secret").await.unwrap();

    let events = tetanus_session::replay(&h.log_path).unwrap();
    let recorded: Vec<String> = events
        .iter()
        .filter(|e| e.ty == topic::USER_MESSAGE)
        .map(|e| e.data["content"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(recorded, vec!["[redacted]"]);

    let requests = requests.lock().expect("requests").clone();
    let sent: Vec<&str> = requests[0]
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(sent, vec!["[redacted]"]);

    let journal = std::fs::read_to_string(&h.log_path).unwrap();
    assert!(
        !journal.contains("my secret"),
        "the original never reaches the journal"
    );
}

/// TC-PORT-LOOP-4: an empty assembly omits the system message entirely.
///
/// Upstream: `loop.spec.ts`, "omits the system field when
/// system-prompt/assemble short-circuits with an empty assembly".
///
/// Input: a `system-prompt/assemble` listener that returns no sections and
/// keeps the tools.
/// Expected: no `system` message in any request, and the turn still runs its
/// two steps.
#[tokio::test]
async fn an_empty_assembly_omits_the_system_message() {
    let h = Harness::new("port-empty-prompt").await;
    let (requests, _record) = record_requests(h.bus());

    let _empty = h.bus().on_waterfall::<AssemblePrompt, _>(|ev, _next| {
        Box::pin(async move {
            SystemPrompt {
                sections: Vec::new(),
                tools: std::mem::take(&mut ev.tools),
                variables: std::mem::take(&mut ev.variables),
            }
        })
    });

    let outcome = h.engine.run_turn("no system prompt").await.unwrap();

    let requests = requests.lock().expect("requests").clone();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!(
            !request.messages.iter().any(|m| m.role == Role::System),
            "an empty assembly sends no system message"
        );
    }
    assert_eq!(outcome.steps, 2, "the turn is unaffected otherwise");
}

/// TC-PORT-LOOP-5: the offered tool order is canonical, not registration order.
///
/// Upstream: `tool-order.spec.ts`, "produces the same header order for any
/// registration order".
///
/// Input: two engines holding the same three tools, registered in opposite
/// orders.
/// Expected: both request the same canonical order. What a configured order
/// does instead is TC-PORT-LOOP-9.
#[tokio::test]
async fn registration_order_does_not_change_the_offered_tools() {
    let names = |registry: ToolRegistry| async {
        let h = Harness::with_tools("port-order", registry).await;
        let (requests, _record) = record_requests(h.bus());
        h.engine.run_turn("what can you do").await.unwrap();
        let requests = requests.lock().expect("requests").clone();
        requests[0]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>()
    };

    let forward = names(
        ToolRegistry::new()
            .with(Arc::new(Named("alpha")))
            .with(Arc::new(EchoTool))
            .with(Arc::new(Named("zulu"))),
    )
    .await;
    let backward = names(
        ToolRegistry::new()
            .with(Arc::new(Named("zulu")))
            .with(Arc::new(EchoTool))
            .with(Arc::new(Named("alpha"))),
    )
    .await;

    assert_eq!(forward, vec!["alpha", "echo", "zulu"]);
    assert_eq!(
        backward, forward,
        "the order is the tools', not the wiring's"
    );
}

/// TC-PORT-LOOP-6: a gate that refuses a call keeps the body from running, and
/// the refusal is what the model sees next.
///
/// Upstream: `interception.spec.ts`, "deny short-circuits dispatch into an
/// isError result the model sees". Upstream denies on its `tools/pre-execute`
/// gate; tetanus's `tools/pre-execute` rewrites the call and `tools/execute`
/// wraps the dispatch, so the refusing listener sits on `tools/execute`.
///
/// Expected: the tool body runs zero times; `tool/result` records `ok: false`
/// with the reason; the next request carries the refusal; the turn still ends
/// naturally.
#[tokio::test]
async fn a_refused_call_never_runs_and_the_model_sees_why() {
    let ran = Arc::new(AtomicU32::new(0));
    let h = Harness::with_tools(
        "port-refused",
        ToolRegistry::new().with(Arc::new(Counting(Arc::clone(&ran)))),
    )
    .await;
    let (requests, _record) = record_requests(h.bus());

    let _gate = h.bus().on_waterfall::<ToolsExecute, _>(|ev, _next| {
        Box::pin(async move {
            Err(ToolError::Failed(
                ev.call.name.clone(),
                "permission denied".into(),
            ))
        })
    });

    let outcome = h.engine.run_turn("do the thing").await.unwrap();

    assert_eq!(ran.load(Ordering::Relaxed), 0, "the body never ran");

    let result = tetanus_session::replay(&h.log_path)
        .unwrap()
        .into_iter()
        .find(|e| e.ty == topic::TOOL_RESULT)
        .expect("tool/result");
    assert_eq!(result.data["ok"], false);
    let recorded = result.data["content"].as_str().unwrap_or_default();
    assert!(recorded.contains("permission denied"), "{recorded}");

    let requests = requests.lock().expect("requests").clone();
    let seen = requests[1]
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("the model is told");
    assert!(seen.content.contains("permission denied"), "{seen:?}");
    assert_eq!(outcome.reason, StopReason::Natural);
}

/// TC-PORT-LOOP-7: a provider failure ends that turn and no more.
///
/// Upstream: `loop.spec.ts`, "contains a strict-variable render failure: the
/// turn errors, the loop keeps serving turns", and `request-error.spec.ts`.
///
/// Input: an `llm/stream` listener that fails the first turn's call and
/// delegates afterwards.
/// Expected: the first turn returns `TurnError::Llm` carrying the provider
/// message, its attempt is on the journal, and the next turn on the same engine
/// runs normally as turn 2. Upstream additionally closes the failed turn with a
/// durable boundary; tetanus does not yet, and that gap is a row in
/// `docs/parity.md` section 3.
#[tokio::test]
async fn a_provider_failure_ends_the_turn_not_the_engine() {
    let h = Harness::new("port-provider-error").await;

    let failing = Arc::new(AtomicBool::new(true));
    let armed = Arc::clone(&failing);
    let _boom = h.bus().on_waterfall::<LlmStream, _>(move |ev, next| {
        let armed = Arc::clone(&armed);
        Box::pin(async move {
            if armed.swap(false, Ordering::Relaxed) {
                return Err(LlmError::Provider {
                    status: 503,
                    message: "upstream is down".into(),
                    retry_after_ms: None,
                });
            }
            next.run(ev).await
        })
    });

    let failed = h.engine.run_turn("first").await;
    match failed {
        Err(TurnError::Llm(err)) => assert!(err.to_string().contains("upstream is down")),
        other => panic!("expected a provider error, got {other:?}"),
    }

    let recovered = h.engine.run_turn("run one full turn").await.unwrap();
    assert_eq!(recovered.turn, 2, "turn numbering survives the failure");
    assert_eq!(recovered.reason, StopReason::Natural);
    assert_eq!(recovered.content, "You said: run one full turn");

    let starts = tetanus_session::replay(&h.log_path)
        .unwrap()
        .into_iter()
        .filter(|e| e.ty == topic::TURN_START)
        .count();
    assert_eq!(starts, 2, "the failed attempt is on the journal too");
}

/// TC-PORT-LOOP-8: a replayed journal derives the same history as the live log.
///
/// Upstream: `loop.spec.ts`, "replays a session log into an identical derived
/// history".
///
/// Input: one mock turn, flushed, then read back from disk.
/// Expected: the derived history is identical either way, and it is the history
/// the second request was actually built from.
#[tokio::test]
async fn a_replayed_journal_derives_the_same_history() {
    let h = Harness::new("port-replay-history").await;
    let (requests, _record) = record_requests(h.bus());

    h.engine.run_turn("replay me").await.unwrap();
    h.engine.flush().await.unwrap();

    let live = derive_messages(&h.engine.log().events());
    let replayed = derive_messages(&tetanus_session::replay(&h.log_path).unwrap());
    assert_eq!(live, replayed, "replay is re-derivation, not a second copy");

    let requests = requests.lock().expect("requests").clone();
    let sent: Vec<Message> = requests[1]
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .cloned()
        .collect();
    assert_eq!(
        sent,
        live[..sent.len()].to_vec(),
        "the request carried that derived history"
    );
}

/// TC-PORT-LOOP-9: a configured order decides what the model is offered, and
/// the tools it does not name go to the rest entry in canonical order.
///
/// Upstream: `tool-order.spec.ts`, "honors a configured toolOrder in the logged
/// header and the dispatched request", and the system-prompt suite's "applies a
/// configured toolOrder: listed positions, rest at the rest entry
/// lexicographically". This is also the one place the rest entry's value is
/// pinned; everything else names the constant.
///
/// Input: four registered tools, and the order `todo_write`, rest, `bash`.
/// Expected: the rest entry is `<unlisted-tools>`, and the request offers
/// `todo_write`, `echo_a`, `echo_b`, `bash` - the two unlisted tools
/// lexicographically, in the one place the order left for them.
#[tokio::test]
async fn a_configured_order_places_the_tools_it_names_and_pools_the_rest() {
    assert_eq!(TOOL_ORDER_REST, "<unlisted-tools>");

    let tools = ToolRegistry::new()
        .with(Arc::new(Named("bash")))
        .with(Arc::new(Named("echo_b")))
        .with(Arc::new(Named("todo_write")))
        .with(Arc::new(Named("echo_a")));
    let order = ToolOrder::new(["todo_write", TOOL_ORDER_REST, "bash"], &tools).expect("order");
    let config = TurnConfig {
        tool_order: Some(order),
        ..TurnConfig::default()
    };

    let h = Harness::with_config("port-order-configured", tools, config).await;
    let (requests, _record) = record_requests(h.bus());
    h.engine.run_turn("what can you do").await.unwrap();

    let requests = requests.lock().expect("requests").clone();
    assert_eq!(
        offered(&requests[0]),
        vec!["todo_write", "echo_a", "echo_b", "bash"]
    );
}

/// TC-PORT-LOOP-10: an order that names a tool nobody registered is refused,
/// and the refusal names every such tool and what is registered instead.
///
/// Upstream: `tool-order.spec.ts`, "closes a no-step turn when toolOrder names
/// an unregistered tool", and the system-prompt suite's two refusal cases.
/// Upstream can only find this while a turn assembles, because its plugins
/// register tools later, so the turn opens and closes with no step. A tetanus
/// registry is settled first, so the refusal comes earlier: the order is
/// unbuildable, and no engine exists to start a turn on.
///
/// Input: two ghost names against a registry of two tools, then one ghost name
/// against an empty registry.
/// Expected: both refusals are `Unregistered`, listing the ghosts in the order
/// they were configured, and naming the registered tools - `(none)` when there
/// are none.
#[test]
fn an_order_naming_an_unregistered_tool_is_refused_before_any_turn() {
    let tools = ToolRegistry::new()
        .with(Arc::new(Named("bash")))
        .with(Arc::new(Named("todo_write")));
    let refused = ToolOrder::new(["todo_write", "ghost", TOOL_ORDER_REST, "wraith"], &tools)
        .expect_err("two names nobody registered");
    assert!(matches!(refused, ToolOrderError::Unregistered { .. }));
    assert_eq!(
        refused.to_string(),
        r#"tool order lists unregistered tools "ghost", "wraith"; registered: bash, todo_write"#
    );

    let refused = ToolOrder::new(["ghost", TOOL_ORDER_REST], &ToolRegistry::new())
        .expect_err("one name, and nothing registered at all");
    assert_eq!(
        refused.to_string(),
        r#"tool order lists unregistered tool "ghost"; registered: (none)"#
    );
}

/// TC-PORT-LOOP-11: an order with no rest entry, or with one name twice, is
/// refused.
///
/// Upstream: `tool-order.spec.ts`, "rejects %s at load (the rest entry is
/// required)" and "rejects %s at load", both of which fail the plugin's own
/// construction.
///
/// Input: four orders - empty, no rest entry, a name twice, and the rest entry
/// twice.
/// Expected: the first two are `NoRest`, the last two are `Duplicate` naming the
/// repeated entry. A tool order is unbuildable in each case, so nothing has to
/// decide later what half of it meant.
#[test]
fn an_order_without_a_rest_entry_or_with_a_repeat_is_refused() {
    let tools = ToolRegistry::new()
        .with(Arc::new(Named("bash")))
        .with(Arc::new(Named("todo_write")));
    let refuse = |names: Vec<&str>| ToolOrder::new(names, &tools).expect_err("refused");

    let empty = refuse(vec![]);
    assert!(matches!(empty, ToolOrderError::NoRest));
    assert_eq!(
        empty.to_string(),
        r#"tool order must contain the "<unlisted-tools>" entry, which is where the tools it does not name go"#
    );
    assert!(matches!(
        refuse(vec!["bash", "todo_write"]),
        ToolOrderError::NoRest
    ));

    let twice = refuse(vec!["bash", "bash", TOOL_ORDER_REST]);
    assert!(matches!(&twice, ToolOrderError::Duplicate(name) if name == "bash"));
    assert_eq!(
        twice.to_string(),
        r#"tool order lists "bash" more than once"#
    );
    assert!(matches!(
        refuse(vec![TOOL_ORDER_REST, "bash", TOOL_ORDER_REST]),
        ToolOrderError::Duplicate(name) if name == TOOL_ORDER_REST
    ));
}

/// TC-PORT-LOOP-12: a registry holding a tool named like the rest entry is
/// refused.
///
/// Upstream: `tool-order.spec.ts`, "rejects a provider tool named like the
/// reserved rest entry". Upstream refuses it whether or not an order is
/// configured, because assembly always looks. tetanus looks when an order is
/// read: with no order the name is one more tool name, and reserving it against
/// a harness that will never arrange anything buys nothing.
///
/// Input: a registry holding `<unlisted-tools>`, and an order that is nothing
/// but the rest entry.
/// Expected: `Reserved`, so a tool cannot take the place the order keeps for
/// everything it did not name.
#[test]
fn a_tool_named_like_the_rest_entry_is_refused() {
    let tools = ToolRegistry::new().with(Arc::new(Named(TOOL_ORDER_REST)));
    let refused = ToolOrder::new([TOOL_ORDER_REST], &tools).expect_err("the reserved name");
    assert!(matches!(refused, ToolOrderError::Reserved));
    assert_eq!(
        refused.to_string(),
        r#"a registered tool is named "<unlisted-tools>", which a tool order keeps for its rest entry"#
    );
}

/// TC-PORT-LOOP-13: the order is applied before `system-prompt/assemble`, and a
/// tool a listener adds there keeps the place that listener gave it.
///
/// Upstream: `tool-order.spec.ts`, "canonicalizes BEFORE the assemble
/// waterfall: listeners see the ordered list and own their own edits".
///
/// Input: a configured order, and a listener that records what it was handed
/// and appends one more tool.
/// Expected: the listener sees the configured order, and the request offers that
/// order with the appended tool last - the harness orders what the registry
/// contributed, and a listener owns the determinism of what it emits.
#[tokio::test]
async fn the_order_is_settled_before_the_assemble_waterfall() {
    let tools = ToolRegistry::new()
        .with(Arc::new(Named("alpha")))
        .with(Arc::new(Named("zulu")));
    let order = ToolOrder::new(["zulu", TOOL_ORDER_REST], &tools).expect("order");
    let config = TurnConfig {
        tool_order: Some(order),
        ..TurnConfig::default()
    };

    let h = Harness::with_config("port-order-waterfall", tools, config).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _append = h.bus().on_waterfall::<AssemblePrompt, _>(move |ev, next| {
        sink.lock()
            .expect("seen")
            .push(ev.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>());
        ev.tools.push(Named("aardvark").schema());
        Box::pin(next.run(ev))
    });
    let (requests, _record) = record_requests(h.bus());

    h.engine.run_turn("what can you do").await.unwrap();

    let seen = seen.lock().expect("seen").clone();
    assert_eq!(seen[0], vec!["zulu", "alpha"]);
    let requests = requests.lock().expect("requests").clone();
    assert_eq!(offered(&requests[0]), vec!["zulu", "alpha", "aardvark"]);
}

/// The tool names one request offers, in the order it offers them.
fn offered(request: &ModelRequest) -> Vec<String> {
    request.tools.iter().map(|t| t.name.clone()).collect()
}

/// Record every request the driver builds, in step order.
fn record_requests(bus: &EventBus) -> (Arc<Mutex<Vec<ModelRequest>>>, EffectHandle) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let handle = bus.on_waterfall::<AgentRequest, _>(move |ev, next| {
        let sink = Arc::clone(&sink);
        Box::pin(async move {
            let request = next.run(ev).await;
            sink.lock().expect("requests").push(request.clone());
            request
        })
    });
    (seen, handle)
}

fn position(trace: &[String], topic: &str) -> usize {
    trace
        .iter()
        .position(|t| t == topic)
        .unwrap_or_else(|| panic!("{topic} is in the trace"))
}

/// A tool that only has to exist, so a case can talk about ordering.
struct Named(&'static str);

#[async_trait::async_trait]
impl Tool for Named {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.0.to_string(),
            description: format!("The {} tool.", self.0),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }
    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok(self.0))
    }
}

/// Stands in for `echo`, and counts how often its body actually runs.
struct Counting(Arc<AtomicU32>);

#[async_trait::async_trait]
impl Tool for Counting {
    fn schema(&self) -> ToolSchema {
        EchoTool.schema()
    }
    async fn execute(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        EchoTool.execute(arguments).await
    }
}
