//! Test Design Specification: upstream tool-call scheduling, ported.
//!
//! Feature under test: how one step's tool calls are scheduled - which calls
//! may overlap, which run alone, how many run at once, and the order their
//! results are committed in. Upstream pins the same rules in
//! `packages/core/agent-loop/tests/tool-calls.spec.ts`; each case names the
//! upstream case it comes from.
//!
//! Approach: the offline fixture the other turn suites use, with the provider
//! replaced by an `llm/stream` listener that asks for a chosen list of calls,
//! and tools that record when they start and end. Ordering is made
//! deterministic by yield count, not by clock: a call that yields fewer times
//! settles first on the single-threaded test runtime, so no case sleeps.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

// This suite drives the fixture's engine with its own provider and tools; a
// test binary lints the parts of a shared fixture it does not reach for.
#[allow(dead_code)]
mod harness;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use harness::Harness;
use serde_json::json;
use tetanus_core::{EffectHandle, EventBus};
use tetanus_turn::events::LlmStream;
use tetanus_turn::llm::ModelResponse;
use tetanus_turn::log::topic;
use tetanus_turn::tools::{
    Tool, ToolCall, ToolError, ToolMode, ToolOutcome, ToolRegistry, ToolSchema,
};
use tetanus_turn::TurnConfig;

/// TC-PORT-TOOL-1: parallel-safe siblings run at the same time.
///
/// Upstream: `tool-calls.spec.ts`, "runs concurrency-safe calls in parallel".
///
/// Input: three calls on the parallel-safe tool, in one step.
/// Expected: all three are in flight at once, and their results are on the
/// journal in the order the model asked for them.
#[tokio::test]
async fn parallel_safe_siblings_run_at_the_same_time() {
    let probes = Probes::default();
    let h = Harness::with_tools("tool-parallel", probes.registry()).await;
    let _provider = asks_for(h.bus(), vec![safe("a", 1), safe("b", 1), safe("c", 1)]);

    h.engine.run_turn("three at once").await.unwrap();

    assert_eq!(probes.peak(), 3, "every sibling overlapped the others");
    assert_eq!(committed(&h.log_path), ["a", "b", "c"]);
}

/// TC-PORT-TOOL-2: an unsafe call is a barrier.
///
/// Upstream: `tool-calls.spec.ts`, "runs a non-concurrency-safe call
/// exclusively, after earlier calls settle and before later ones start".
///
/// Input: two parallel-safe calls, then one exclusive call, then one more
/// parallel-safe call.
/// Expected: three groups. Nothing overlaps the exclusive call, so at most two
/// calls are ever in flight; both earlier calls end before it starts, and it
/// ends before the later call starts.
#[tokio::test]
async fn an_unsafe_call_runs_alone_between_its_siblings() {
    let probes = Probes::default();
    let h = Harness::with_tools("tool-barrier", probes.registry()).await;
    let _provider = asks_for(
        h.bus(),
        vec![safe("a", 1), safe("b", 1), sole("x", 1), safe("c", 1)],
    );

    h.engine
        .run_turn("one at a time in the middle")
        .await
        .unwrap();

    let trace = probes.trace();
    assert_eq!(probes.peak(), 2, "the barrier never shared the step");
    assert!(at(&trace, "end:a") < at(&trace, "start:x"));
    assert!(at(&trace, "end:b") < at(&trace, "start:x"));
    assert!(at(&trace, "end:x") < at(&trace, "start:c"));
    assert_eq!(committed(&h.log_path), ["a", "b", "x", "c"]);
}

/// TC-PORT-TOOL-3: results are committed in model order, however they settled.
///
/// Upstream: `tool-calls.spec.ts`, "emits results in the order the model asked
/// for them, not the order they finished".
///
/// Input: three parallel-safe calls whose yield counts make the last one finish
/// first and the first one finish last.
/// Expected: the tools finished in reverse, and the journal still reads in
/// model order. A resumed transcript must not depend on which call was quicker.
#[tokio::test]
async fn results_commit_in_model_order_not_completion_order() {
    let probes = Probes::default();
    let h = Harness::with_tools("tool-order", probes.registry()).await;
    let _provider = asks_for(h.bus(), vec![safe("a", 6), safe("b", 3), safe("c", 0)]);

    h.engine.run_turn("out of order").await.unwrap();

    assert_eq!(
        probes.ends(),
        ["c", "b", "a"],
        "the quickest finished first"
    );
    assert_eq!(committed(&h.log_path), ["a", "b", "c"]);
}

/// TC-PORT-TOOL-4: the pool starts at most `max_parallel_tool_calls`, and
/// replenishes as calls settle.
///
/// Upstream: `tool-calls.spec.ts`, "respects maxParallelToolCalls".
///
/// Input: five parallel-safe calls under a cap of two.
/// Expected: never more than two in flight, all five still run, and the results
/// are in model order. A cap is a limit on overlap, not on how many calls a
/// step may make. Which of the two in flight settles first is the pool's
/// business, so the case sorts the finishes rather than pinning their order.
#[tokio::test]
async fn the_pool_never_exceeds_the_cap_and_still_runs_every_call() {
    let probes = Probes::default();
    let h = Harness::with_config("tool-cap", probes.registry(), capped(2)).await;
    let _provider = asks_for(
        h.bus(),
        vec![
            safe("a", 1),
            safe("b", 1),
            safe("c", 1),
            safe("d", 1),
            safe("e", 1),
        ],
    );

    h.engine.run_turn("five under a cap of two").await.unwrap();

    let mut finished = probes.ends();
    finished.sort();
    assert_eq!(probes.peak(), 2, "the cap held");
    assert_eq!(finished, ["a", "b", "c", "d", "e"], "the pool replenished");
    assert_eq!(committed(&h.log_path), ["a", "b", "c", "d", "e"]);
}

/// TC-PORT-TOOL-5: a cap of one is fully serial dispatch.
///
/// Upstream: `tool-calls.spec.ts`, "maxParallelToolCalls of 1 serializes
/// everything".
///
/// Input: three parallel-safe calls under a cap of one.
/// Expected: one call in flight at a time, and each ends before the next
/// starts, so a deployment can turn overlap off without changing its tools.
#[tokio::test]
async fn a_cap_of_one_is_serial_dispatch() {
    let probes = Probes::default();
    let h = Harness::with_config("tool-serial", probes.registry(), capped(1)).await;
    let _provider = asks_for(h.bus(), vec![safe("a", 2), safe("b", 2), safe("c", 2)]);

    h.engine.run_turn("one at a time").await.unwrap();

    assert_eq!(probes.peak(), 1);
    assert_eq!(
        probes.trace(),
        ["start:a", "end:a", "start:b", "end:b", "start:c", "end:c"]
    );
    assert_eq!(committed(&h.log_path), ["a", "b", "c"]);
}

// ---------------------------------------------------------------- fixtures

/// The parallel-safe tool. Its calls may overlap.
const SAFE: &str = "safe";
/// The exclusive tool. Its calls are barriers.
const SOLE: &str = "sole";

/// What the recording tools share: one ordered trace, plus the live count and
/// the highest live count the step ever reached.
#[derive(Clone, Default)]
struct Probes {
    trace: Arc<Mutex<Vec<String>>>,
    live: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl Probes {
    fn registry(&self) -> ToolRegistry {
        ToolRegistry::new()
            .with(Arc::new(Probe {
                name: SAFE,
                mode: ToolMode::Parallel,
                probes: self.clone(),
            }))
            .with(Arc::new(Probe {
                name: SOLE,
                mode: ToolMode::Exclusive,
                probes: self.clone(),
            }))
    }

    fn trace(&self) -> Vec<String> {
        self.trace.lock().expect("trace").clone()
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    /// The call ids in the order their bodies finished.
    fn ends(&self) -> Vec<String> {
        self.trace()
            .iter()
            .filter_map(|note| note.strip_prefix("end:").map(str::to_string))
            .collect()
    }

    fn note(&self, note: String) {
        self.trace.lock().expect("trace").push(note);
    }
}

/// A tool that records its own overlap. `yields` decides how long it stays in
/// flight: on the single-threaded test runtime, fewer yields means it settles
/// sooner, with no clock involved.
struct Probe {
    name: &'static str,
    mode: ToolMode,
    probes: Probes,
}

#[async_trait::async_trait]
impl Tool for Probe {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.into(),
            description: "Record when this call starts and ends.".into(),
            parameters: json!({ "type": "object" }),
        }
    }

    fn mode(&self, _arguments: &serde_json::Value) -> ToolMode {
        self.mode
    }

    async fn execute(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        let id = arguments["id"].as_str().unwrap_or_default().to_string();
        let live = self.probes.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.probes.peak.fetch_max(live, Ordering::SeqCst);
        self.probes.note(format!("start:{id}"));

        for _ in 0..arguments["yields"].as_u64().unwrap_or_default() {
            tokio::task::yield_now().await;
        }

        self.probes.note(format!("end:{id}"));
        self.probes.live.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutcome::ok(id))
    }
}

/// Replace the provider: the first request asks for `calls`, and every later
/// one answers, so the turn ends after the tools have run.
fn asks_for(bus: &EventBus, calls: Vec<ToolCall>) -> EffectHandle {
    let pending = Arc::new(Mutex::new(Some(calls)));
    bus.on_waterfall::<LlmStream, _>(move |_ev, _next| {
        let asked = pending.lock().expect("calls").take().unwrap_or_default();
        Box::pin(async move {
            Ok(ModelResponse {
                content: if asked.is_empty() { "done" } else { "" }.into(),
                tool_calls: asked,
                finish_reason: "stop".into(),
                ..Default::default()
            })
        })
    })
}

fn safe(id: &str, yields: u64) -> ToolCall {
    call(id, SAFE, yields)
}

fn sole(id: &str, yields: u64) -> ToolCall {
    call(id, SOLE, yields)
}

fn call(id: &str, name: &str, yields: u64) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: json!({ "id": id, "yields": yields }),
    }
}

fn capped(calls: usize) -> TurnConfig {
    TurnConfig {
        max_parallel_tool_calls: calls.try_into().expect("a cap of at least one"),
        ..TurnConfig::default()
    }
}

/// The call ids in the order their results were committed to the journal.
fn committed(log_path: &Path) -> Vec<String> {
    tetanus_session::replay(log_path)
        .expect("replay")
        .into_iter()
        .filter(|event| event.ty == topic::TOOL_RESULT)
        .map(|event| {
            event.data["call_id"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

fn at(trace: &[String], note: &str) -> usize {
    trace
        .iter()
        .position(|seen| seen == note)
        .unwrap_or_else(|| panic!("{note} is missing from {trace:?}"))
}
