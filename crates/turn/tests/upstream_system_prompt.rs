//! Test Design Specification: upstream system-prompt assembly, ported.
//!
//! Feature under test: `system-prompt/assemble`, the waterfall that decides
//! what the model is told and which tools it may call. Upstream pins the same
//! decisions in `packages/core/system-prompt/tests/system-prompt.spec.ts`; each
//! case names the upstream case it comes from.
//!
//! Approach: the same offline fixture the turn-flow suite uses, driven through
//! the bus only. Upstream's assembly carries surfaces tetanus has not built -
//! a named section registry with an explicit order, prompt variables and
//! `{{name}}` interpolation, runtime-context providers, and "complete"
//! sections that replace the assembly. Cases that only exist because of those
//! are not restated here as passing tests; they stay rows in `docs/parity.md`.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

// This suite uses the fixture's engine and bus, not its trace constants; a
// test binary lints the parts of a shared fixture it does not reach for.
#[allow(dead_code)]
mod harness;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use harness::Harness;
use tetanus_core::{EffectHandle, EventBus};
use tetanus_turn::events::{AgentRequest, AssemblePrompt, PromptSection, SystemPrompt};
use tetanus_turn::llm::{ModelRequest, Role};

/// TC-PORT-PROMPT-1: sections reach the model in the order they were
/// contributed, joined by a blank line, and the registry's tool schemas ride
/// the same assembly.
///
/// Upstream: "assembles sections in order with context-resolved text and
/// collected tools".
///
/// Translation: upstream orders by an explicit numeric `order` on a named
/// section. tetanus has no section registry, so the order under test is the
/// order of `AssemblePrompt.sections`, which is what the waterfall preserves.
///
/// Expected: base, then the first contributor, then the second; the request's
/// system message is those three joined by a blank line; and `echo` is offered.
#[tokio::test]
async fn sections_reach_the_model_in_order_with_the_tools() {
    let h = Harness::new("prompt-order").await;
    let (requests, _record) = record_requests(h.bus());
    let _first = contribute(h.bus(), "first", "FIRST");
    let _second = contribute(h.bus(), "second", "SECOND");

    h.engine.run_turn("order").await.unwrap();

    let requests = requests.lock().expect("requests").clone();
    let system = system_message(&requests[0]);
    let base = tetanus_turn::TurnConfig::default().base_prompt;
    assert_eq!(system, format!("{base}\n\nFIRST\n\nSECOND"));
    assert!(
        requests[0].tools.iter().any(|t| t.name == "echo"),
        "the registry's schemas travel with the prompt: {:?}",
        requests[0].tools
    );
}

/// TC-PORT-PROMPT-2: a section with no text contributes nothing, not a gap.
///
/// Upstream: "filters out empty section text from renderPrompt", and "renders
/// no persona section for a persona-less deployment (empty default)".
///
/// Input: an empty section contributed between two that have text.
/// Expected: exactly one blank line between the two real sections. Before this
/// case the empty section widened the gap to two, so a deployment that left a
/// section unfilled shipped the hole to the model.
#[tokio::test]
async fn an_empty_section_contributes_nothing() {
    let h = Harness::new("prompt-empty-section").await;
    let (requests, _record) = record_requests(h.bus());
    let _silent = contribute(h.bus(), "persona", "");
    let _after = contribute(h.bus(), "after", "AFTER");

    h.engine.run_turn("empty section").await.unwrap();

    let system = system_message(&requests.lock().expect("requests")[0]);
    let base = tetanus_turn::TurnConfig::default().base_prompt;
    assert_eq!(system, format!("{base}\n\nAFTER"));
    assert!(
        !system.contains("\n\n\n"),
        "an unfilled section leaves no hole: {system:?}"
    );
}

/// TC-PORT-PROMPT-3: an assembly whose sections all render empty puts no
/// system message on the request.
///
/// Upstream: "filters empty context, interpolates variables, and returns empty
/// without active context" - `renderPrompt` returns `''` when everything is
/// empty, and an empty prompt is not sent.
///
/// This is the case the filter exists for: without it, two empty sections
/// render as `"\n\n"`, which is not empty, so a whitespace-only system message
/// reaches the provider.
///
/// Expected: no message with role `system`, and the first message is the
/// user's.
#[tokio::test]
async fn an_all_empty_assembly_sends_no_system_message() {
    let h = Harness::new("prompt-all-empty").await;
    let (requests, _record) = record_requests(h.bus());
    let _blank = h.bus().on_waterfall::<AssemblePrompt, _>(|ev, next| {
        for section in &mut ev.sections {
            section.text.clear();
        }
        ev.sections.push(PromptSection {
            id: "also-empty".into(),
            text: String::new(),
        });
        Box::pin(next.run(ev))
    });

    h.engine.run_turn("all empty").await.unwrap();

    let requests = requests.lock().expect("requests").clone();
    assert!(
        !requests[0].messages.iter().any(|m| m.role == Role::System),
        "an empty prompt is not a message: {:?}",
        requests[0].messages
    );
    assert_eq!(requests[0].messages[0].role, Role::User);
}

/// TC-PORT-PROMPT-4: several `system-prompt/assemble` listeners compose, in
/// registration order, each seeing what the ones before it left.
///
/// Upstream: "composes multiple system-prompt/assemble waterfall listeners in
/// order, with the context".
///
/// Expected: the listener registered first is the outermost, so it observes
/// the section the second one added, and both coordinates reach both.
#[tokio::test]
async fn assemble_listeners_compose_in_registration_order() {
    let h = Harness::new("prompt-compose").await;
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let outer = Arc::clone(&seen);
    let _first = h.bus().on_waterfall::<AssemblePrompt, _>(move |ev, next| {
        outer.lock().expect("seen").push("outer before".into());
        let outer = Arc::clone(&outer);
        Box::pin(async move {
            let prompt = next.run(ev).await;
            let ids: Vec<&str> = prompt.sections.iter().map(|s| s.id.as_str()).collect();
            outer
                .lock()
                .expect("seen")
                .push(format!("outer after: {}", ids.join(",")));
            prompt
        })
    });

    let inner = Arc::clone(&seen);
    let _second = h.bus().on_waterfall::<AssemblePrompt, _>(move |ev, next| {
        inner
            .lock()
            .expect("seen")
            .push(format!("inner at {}/{}", ev.turn, ev.step));
        ev.sections.push(PromptSection {
            id: "inner".into(),
            text: "INNER".into(),
        });
        Box::pin(next.run(ev))
    });

    h.engine.run_turn("compose").await.unwrap();

    let seen = seen.lock().expect("seen").clone();
    assert_eq!(
        &seen[..3],
        &[
            "outer before".to_string(),
            "inner at 1/1".to_string(),
            "outer after: base,inner".to_string(),
        ],
        "the first registration wraps the second"
    );
}

/// TC-PORT-PROMPT-5: a listener that does not call `next` short-circuits the
/// assembly.
///
/// Upstream: "lets a waterfall listener short-circuit by not calling next()".
///
/// Expected: the returned prompt is the short-circuiting listener's own, the
/// listeners it wraps never run, and the engine's terminal never contributes
/// the base section.
#[tokio::test]
async fn a_listener_that_skips_next_short_circuits_the_assembly() {
    let h = Harness::new("prompt-short-circuit").await;
    let (requests, _record) = record_requests(h.bus());

    let _stop = h.bus().on_waterfall::<AssemblePrompt, _>(|_ev, _next| {
        Box::pin(async move {
            SystemPrompt {
                sections: vec![PromptSection {
                    id: "only".into(),
                    text: "ONLY".into(),
                }],
                tools: Vec::new(),
            }
        })
    });
    let inner_runs = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&inner_runs);
    let _inner = h.bus().on_waterfall::<AssemblePrompt, _>(move |ev, next| {
        counted.fetch_add(1, Ordering::Relaxed);
        Box::pin(next.run(ev))
    });

    h.engine.run_turn("short circuit").await.unwrap();

    let requests = requests.lock().expect("requests").clone();
    assert_eq!(system_message(&requests[0]), "ONLY");
    assert!(
        requests[0].tools.is_empty(),
        "the short-circuiting listener decides the tools too"
    );
    assert_eq!(
        inner_runs.load(Ordering::Relaxed),
        0,
        "a listener behind the short circuit never runs"
    );
}

/// TC-PORT-PROMPT-6: dropping the handle removes the contribution.
///
/// Upstream: "removes contributions when the contributing fiber is disposed
/// (HMR safety)", and "removes section when returned disposer is called
/// directly".
///
/// Translation: upstream disposes a fiber; tetanus registrations are RAII, so
/// the equivalent is dropping the `EffectHandle`.
///
/// Expected: the section is in the first turn's prompt and gone from the
/// second's, and nothing else about the prompt changes.
#[tokio::test]
async fn dropping_the_handle_removes_the_contribution() {
    let h = Harness::new("prompt-dispose").await;
    let (requests, _record) = record_requests(h.bus());
    let plugin = contribute(h.bus(), "plugin", "PLUGIN");

    h.engine.run_turn("with the plugin").await.unwrap();
    drop(plugin);
    h.engine.run_turn("without the plugin").await.unwrap();

    let requests = requests.lock().expect("requests").clone();
    let base = tetanus_turn::TurnConfig::default().base_prompt;
    assert_eq!(system_message(&requests[0]), format!("{base}\n\nPLUGIN"));
    assert_eq!(
        system_message(requests.last().expect("a later request")),
        base,
        "a dropped handle leaves nothing behind"
    );
}

/// TC-PORT-PROMPT-7: the prompt is assembled again for every step, and one
/// step's assembly does not leak into the next.
///
/// Upstream: "resolves section text providers against the assemble context, at
/// each assemble call", and "assembles snapshots so one-step mutations do not
/// leak into future assemblies".
///
/// Input: a contributor that names the step it ran for, and mutates the
/// section vector it was handed.
/// Expected: two assemblies, `1/1` and `1/2`; step 2 carries its own section
/// and not step 1's, so the assembly each step sees is built fresh.
#[tokio::test]
async fn every_step_assembles_afresh() {
    let h = Harness::new("prompt-per-step").await;
    let (requests, _record) = record_requests(h.bus());

    let coordinates: Arc<Mutex<Vec<(u64, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&coordinates);
    let _stamp = h.bus().on_waterfall::<AssemblePrompt, _>(move |ev, next| {
        seen.lock().expect("coordinates").push((ev.turn, ev.step));
        ev.sections.push(PromptSection {
            id: format!("step-{}", ev.step),
            text: format!("STEP {}", ev.step),
        });
        Box::pin(next.run(ev))
    });

    h.engine.run_turn("per step").await.unwrap();

    assert_eq!(
        *coordinates.lock().expect("coordinates"),
        vec![(1, 1), (1, 2)]
    );
    assert_eq!(
        h.trace()
            .iter()
            .filter(|topic| *topic == "system-prompt/assemble")
            .count(),
        2,
        "one assembly per step, and no assembly outside a step"
    );
    let requests = requests.lock().expect("requests").clone();
    assert!(system_message(&requests[0]).ends_with("STEP 1"));
    let second = system_message(&requests[1]);
    assert!(second.ends_with("STEP 2"), "{second:?}");
    assert!(
        !second.contains("STEP 1"),
        "step 1's mutation did not survive into step 2: {second:?}"
    );
}

/// Record every model request the driver builds, in order.
fn record_requests(bus: &EventBus) -> (Arc<Mutex<Vec<ModelRequest>>>, EffectHandle) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let handle = bus.on_waterfall::<AgentRequest, _>(move |ev, next| {
        sink.lock().expect("requests").push(ev.request.clone());
        Box::pin(next.run(ev))
    });
    (seen, handle)
}

/// A plugin that adds one section to every assembly.
fn contribute(bus: &EventBus, id: &str, text: &str) -> EffectHandle {
    let section = PromptSection {
        id: id.to_string(),
        text: text.to_string(),
    };
    bus.on_waterfall::<AssemblePrompt, _>(move |ev, next| {
        ev.sections.push(section.clone());
        Box::pin(next.run(ev))
    })
}

/// The one system message on a request, which every case here asserts against.
fn system_message(request: &ModelRequest) -> String {
    let system: Vec<&str> = request
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(system.len(), 1, "exactly one system message: {system:?}");
    system[0].to_string()
}
