//! Test Design Specification: upstream system-prompt assembly, ported.
//!
//! Feature under test: `system-prompt/assemble`, the waterfall that decides
//! what the model is told and which tools it may call. Upstream pins the same
//! decisions in `packages/core/system-prompt/tests/system-prompt.spec.ts`; each
//! case names the upstream case it comes from.
//!
//! Approach: the same offline fixture the turn-flow suite uses, driven through
//! the bus, plus a bare registry where a case is about registration alone.
//! Upstream's assembly still carries surfaces tetanus has not built - prompt
//! variables and `{{name}}` interpolation, runtime-context providers, and
//! scoped layers. Cases that only exist because of those are not restated here
//! as passing tests; they stay rows in `docs/parity.md`. Upstream's non-finite order is unrepresentable in
//! an `i32`, so that case has nothing to restate.
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
use tetanus_turn::prompt::{AssembleAt, PromptError, PromptRegistry, Section, SectionText};

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

/// TC-PORT-PROMPT-8: registered sections reach the model in ascending order,
/// whatever order they were registered in, and a tie keeps registration order.
///
/// Upstream: "registers the harness identity and the configured deployment
/// persona" and "assembles sections in order with context-resolved text".
///
/// Translation: upstream seeds two built-in slots from its own config; tetanus
/// seeds one, `TurnConfig::base_prompt`, at [`BASE_ORDER`]. Both are the same
/// rule - the harness's own text is a registered section like any other, so a
/// plugin can speak before it or after it by number rather than by luck.
///
/// Expected: the system message is `EARLY`, the base, then `LATE-A`, `LATE-B`.
#[tokio::test]
async fn registered_sections_render_in_ascending_order() {
    let h = Harness::new("prompt-registry-order").await;
    let (requests, _record) = record_requests(h.bus());

    // Registered back to front, and two at one order, to prove neither the
    // registration order nor the tie decides the render order alone.
    let _b = h
        .sections
        .section(Section::new("late-b", 10, "LATE-B"))
        .expect("late-b");
    let _a = h
        .sections
        .section(Section::new("late-a", 10, "LATE-A"))
        .expect("late-a");
    let _early = h
        .sections
        .section(Section::new("early", -500, "EARLY"))
        .expect("early");

    h.engine.run_turn("order").await.unwrap();

    let requests = requests.lock().expect("requests").clone();
    let base = tetanus_turn::TurnConfig::default().base_prompt;
    assert_eq!(
        system_message(&requests[0]),
        format!("EARLY\n\n{base}\n\nLATE-B\n\nLATE-A")
    );
}

/// TC-PORT-PROMPT-9: a section's provider is asked at every assembly, and is
/// told which assembly is asking.
///
/// Upstream: "resolves section text providers against the assemble context, at
/// each assemble call".
///
/// Expected: two assemblies produce two different texts, each naming its own
/// turn and step, so nothing is cached across a step.
#[test]
fn a_section_provider_is_asked_at_every_assembly() {
    let sections = PromptRegistry::new();
    let _live = sections
        .section(Section::new(
            "coordinates",
            0,
            SectionText::provided(|at| format!("turn {} step {}", at.turn, at.step)),
        ))
        .expect("coordinates");

    let first = sections.assemble(&AssembleAt { turn: 1, step: 1 });
    let second = sections.assemble(&AssembleAt { turn: 1, step: 2 });

    assert_eq!(first[0].text, "turn 1 step 1");
    assert_eq!(second[0].text, "turn 1 step 2");
}

/// TC-PORT-PROMPT-10: a duplicate section name is refused, and the text that
/// was already registered stands.
///
/// Upstream: "rejects a duplicate section name (a double-loaded plugin must
/// fail, not double its text)".
///
/// Expected: the second registration is `PromptError::Duplicate` naming the
/// section, and one assembly still holds exactly one `persona`, the first.
#[test]
fn a_duplicate_section_name_is_refused() {
    let sections = PromptRegistry::new();
    let _first = sections
        .section(Section::new("persona", 0, "FIRST"))
        .expect("persona");

    let Err(err) = sections.section(Section::new("persona", 0, "SECOND")) else {
        panic!("a double-loaded plugin must fail, not double its text");
    };

    assert!(
        matches!(err, PromptError::Duplicate(ref id) if id == "persona"),
        "{err}"
    );
    let assembled = sections.assemble(&AssembleAt { turn: 1, step: 1 });
    assert_eq!(assembled.len(), 1);
    assert_eq!(assembled[0].text, "FIRST");
}

/// TC-PORT-PROMPT-11: the engine's own slot cannot be taken by a plugin.
///
/// Upstream has no counterpart: its built-in sections are ordinary names, so a
/// plugin that registers `deployment:persona` collides with the deployment.
/// tetanus reserves the one name the engine fills from `TurnConfig`, so the
/// failure names the reason instead of reading as an accident.
///
/// Expected: `PromptError::Reserved`, and the registry is unchanged.
#[test]
fn the_base_slot_is_reserved_for_the_engine() {
    let sections = PromptRegistry::new();

    let Err(err) = sections.section(Section::new(tetanus_turn::prompt::BASE_SECTION, 0, "MINE"))
    else {
        panic!("the base slot belongs to the engine");
    };

    assert!(matches!(err, PromptError::Reserved(_)), "{err}");
    assert!(sections
        .assemble(&AssembleAt { turn: 1, step: 1 })
        .is_empty());
}

/// TC-PORT-PROMPT-12: dropping the handle takes the section back out.
///
/// Upstream: "removes section when returned disposer is called directly" and
/// "removes contributions when the contributing fiber is disposed".
///
/// Expected: the section is in the assembly before the drop and gone after it,
/// so a plugin's prompt text dies with the plugin.
#[test]
fn dropping_the_handle_removes_the_section() {
    let sections = PromptRegistry::new();
    let live = sections
        .section(Section::new("temporary", 0, "HERE"))
        .expect("temporary");

    assert_eq!(sections.assemble(&AssembleAt { turn: 1, step: 1 }).len(), 1);
    drop(live);

    assert!(sections
        .assemble(&AssembleAt { turn: 1, step: 2 })
        .is_empty());
}

/// TC-PORT-PROMPT-13: a section registered as the whole prompt is what the
/// model reads, and a listener cannot edit it.
///
/// Upstream: "restores one complete section after the assembly waterfall".
///
/// Input: a complete section, and a listener that rewrites every section it is
/// shown and adds one of its own.
/// Expected: the system message is the complete section's text as assembled;
/// the listener still saw the whole assembly, base included; and the tool
/// catalog that rode the same assembly still reaches the model.
#[tokio::test]
async fn a_complete_section_is_the_whole_prompt() {
    let h = Harness::new("prompt-complete").await;
    let (requests, _record) = record_requests(h.bus());
    let _persona = h
        .sections
        .section(Section::new("persona", 10, "Exact prompt.").complete())
        .expect("persona");

    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let shown = Arc::clone(&seen);
    let _meddler = h.bus().on_waterfall::<AssemblePrompt, _>(move |ev, next| {
        shown
            .lock()
            .expect("seen")
            .push(ev.sections.iter().map(|s| s.id.clone()).collect());
        for section in ev.sections.iter_mut() {
            section.text = "mutated".into();
        }
        ev.sections.push(PromptSection {
            id: "late".into(),
            text: "LATE".into(),
        });
        Box::pin(next.run(ev))
    });

    h.engine.run_turn("complete").await.unwrap();

    let requests = requests.lock().expect("requests").clone();
    assert_eq!(system_message(&requests[0]), "Exact prompt.");
    assert_eq!(
        seen.lock().expect("seen")[0],
        ["base", "persona"],
        "the assembly still ran in full"
    );
    assert!(
        requests[0].tools.iter().any(|tool| tool.name == "echo"),
        "the tool catalog rides the same assembly and is not replaced"
    );
}

/// TC-PORT-PROMPT-14: a registry holds one complete section at a time.
///
/// Upstream: "rejects multiple effective complete sections".
///
/// Translation: upstream can hold two, in different scopes, and fails the
/// assembly that finds both effective. tetanus has no scopes, so a second one
/// can only be a mistake, and it is refused at registration - the earliest
/// point at which it is knowable.
///
/// Expected: the second registration is `PromptError::Complete` naming both
/// sections, the refused section is not registered, and the first still holds
/// the slot.
#[test]
fn a_second_complete_section_is_refused() {
    let sections = PromptRegistry::new();
    let _first = sections
        .section(Section::new("first", 10, "first").complete())
        .expect("first");

    let Err(err) = sections.section(Section::new("second", 20, "second").complete()) else {
        panic!("the second complete section was accepted");
    };

    assert!(
        matches!(err, PromptError::Complete { ref held, ref refused }
            if held == "first" && refused == "second"),
        "{err}"
    );
    assert_eq!(
        sections.assemble(&AssembleAt { turn: 1, step: 1 }).len(),
        1,
        "the refused section is not registered"
    );
    assert_eq!(sections.complete_id().as_deref(), Some("first"));
}

/// TC-PORT-PROMPT-15: the whole prompt is given back when the section goes.
///
/// Upstream: the same disposal contract every contribution has.
///
/// Input: a complete section and an ordinary contributor, one turn, then the
/// complete section's handle dropped and another turn.
/// Expected: the first turn reads the complete section alone; the second reads
/// the assembly again, contributor included.
#[tokio::test]
async fn dropping_the_complete_section_gives_the_prompt_back() {
    let h = Harness::new("prompt-complete-dropped").await;
    let (requests, _record) = record_requests(h.bus());
    let persona = h
        .sections
        .section(Section::new("persona", 10, "Exact prompt.").complete())
        .expect("persona");
    let _plugin = contribute(h.bus(), "plugin", "PLUGIN");

    h.engine.run_turn("first").await.unwrap();
    drop(persona);
    h.engine.run_turn("second").await.unwrap();

    let requests = requests.lock().expect("requests").clone();
    assert_eq!(system_message(&requests[0]), "Exact prompt.");
    let after = system_message(requests.last().expect("a later request"));
    assert!(
        after.contains("PLUGIN") && !after.contains("Exact prompt."),
        "the assembly is back: {after}"
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
