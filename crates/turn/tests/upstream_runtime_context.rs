//! Test Design Specification: telling the model where it is, ported.
//!
//! Feature under test: `tetanus_turn::runtime_context` and the engine's use of
//! it - what a turn tells the model about the world outside the conversation,
//! written once per turn as `context/snapshot`.
//!
//! Contract section 4.4.8 settled every rule here before this was built, so
//! each case names the rule it restates rather than an upstream case id.
//! Upstream's equivalent is `ctx.runtimeContext`, whose providers are Cordis
//! services; the shape asserted here is the contract's, which is what the
//! presentation lane reads.
//!
//! Approach: real turns through the shared harness over a temporary journal,
//! because every rule is about what reaches the journal and what a later
//! request derives from it. A rule asserted against the registry alone would
//! not catch the engine writing the snapshot in the wrong place.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime.
//! One case panics on purpose inside a provider.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value.

mod harness;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};

use harness::Harness;
use tetanus_session::SessionEvent;
use tetanus_turn::log::{derive_messages, topic};
use tetanus_turn::runtime_context::{render, ContextPart, ContextProvider};

fn part(name: &str, text: &str) -> ContextPart {
    ContextPart::new(name, text)
}

fn snapshots(events: &[SessionEvent]) -> Vec<&SessionEvent> {
    events
        .iter()
        .filter(|e| e.ty == topic::CONTEXT_SNAPSHOT)
        .collect()
}

fn parts_of(event: &SessionEvent) -> Vec<ContextPart> {
    serde_json::from_value(event.data["parts"].clone()).expect("parts")
}

/// A provider that counts how often it was asked.
struct Counting {
    name: String,
    text: String,
    asked: Arc<AtomicUsize>,
}

impl ContextProvider for Counting {
    fn name(&self) -> &str {
        &self.name
    }
    fn text(&self) -> String {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.text.clone()
    }
}

/// TC-CTX-1: the snapshot is written once per turn, between `turn/start` and
/// the first `step/start`.
///
/// Section 4.4.8 fixes that position. Written after the first step would mean
/// the first request of the turn did not carry it, which is the request that
/// most needs to know what day it is.
#[tokio::test]
async fn a_snapshot_is_written_once_per_turn_before_the_first_step() {
    let h = Harness::new("ctx-position").await;
    h.context.register_static("time", "The date is 2026-08-24.");

    h.engine.run_turn("where am I").await.expect("turn");
    let events = h.engine.log().events();
    let order: Vec<&str> = events.iter().map(|e| e.ty.as_str()).collect();

    assert_eq!(snapshots(&events).len(), 1, "one per turn");
    let start = order.iter().position(|t| *t == topic::TURN_START).unwrap();
    let snap = order
        .iter()
        .position(|t| *t == topic::CONTEXT_SNAPSHOT)
        .expect("a snapshot");
    let step = order.iter().position(|t| *t == topic::STEP_START).unwrap();
    assert!(start < snap && snap < step, "{order:?}");
}

/// TC-CTX-2: the model reads it as a user message, not as system prompt.
///
/// The whole design, and it is a caching decision: a provider caches a prompt
/// by its longest stable prefix, and a sentence saying what time it is changes
/// every turn. Putting it in the system prompt would invalidate the cached
/// prefix on every request of every session.
#[tokio::test]
async fn the_snapshot_reaches_the_model_as_a_user_message() {
    let h = Harness::new("ctx-user-message").await;
    h.context.register_static("time", "The date is 2026-08-24.");

    h.engine.run_turn("hello").await.expect("turn");
    let history = derive_messages(&h.engine.log().events());

    let found = history
        .iter()
        .find(|m| m.content == "The date is 2026-08-24.")
        .expect("the snapshot is in history");
    assert_eq!(found.role, tetanus_turn::llm::Role::User);
}

/// TC-CTX-3: only the newest snapshot travels.
///
/// A turn writes one, so a long session accumulates them, and yesterday's date
/// is worse than no date. The earlier ones stay on the journal - the journal
/// records what happened, and a reader may want to know what the model was
/// told at the time - but they do not travel again.
#[tokio::test]
async fn only_the_newest_snapshot_becomes_history() {
    // A provider whose text changes per turn, which is the case the rule is
    // about.
    struct Changing(Arc<AtomicUsize>);
    impl ContextProvider for Changing {
        fn name(&self) -> &str {
            "clock"
        }
        fn text(&self) -> String {
            format!("tick {}", self.0.fetch_add(1, Ordering::SeqCst))
        }
    }
    let h = Harness::new("ctx-newest").await;
    h.context
        .register(Arc::new(Changing(Arc::new(AtomicUsize::new(0)))));

    h.engine.run_turn("first").await.expect("turn one");
    h.engine.run_turn("second").await.expect("turn two");
    h.engine.run_turn("third").await.expect("turn three");

    let events = h.engine.log().events();
    assert_eq!(snapshots(&events).len(), 3, "all three are on the journal");

    let history = derive_messages(&events);
    let ticks: Vec<&String> = history
        .iter()
        .map(|m| &m.content)
        .filter(|c| c.starts_with("tick "))
        .collect();
    assert_eq!(ticks, [&"tick 2".to_owned()], "only the newest travels");
}

/// TC-CTX-4: a deployment that configures nothing pays nothing.
///
/// Not an empty array, not a record with no parts - no event at all. A journal
/// from a deployment with no providers must be byte-identical to the one it
/// had before this existed, or every such deployment pays for a feature it
/// does not use and every diff of a journal is noisy.
#[tokio::test]
async fn no_providers_writes_no_snapshot() {
    let h = Harness::new("ctx-none").await;
    h.engine.run_turn("hello").await.expect("turn");

    assert!(snapshots(&h.engine.log().events()).is_empty());
}

/// TC-CTX-5: providers that all say nothing also write nothing.
///
/// The same rule reached the other way. A provider installed but silent this
/// turn is indistinguishable to the model from one not installed, and writing
/// an empty snapshot would put a record on the journal that derives to an
/// empty user message - a blank turn in front of the model.
#[tokio::test]
async fn providers_with_nothing_to_say_write_no_snapshot() {
    let h = Harness::new("ctx-silent").await;
    h.context.register_static("time", "");
    h.context.register_static("branch", "");

    h.engine.run_turn("hello").await.expect("turn");
    assert!(snapshots(&h.engine.log().events()).is_empty());
}

/// TC-CTX-6: the parts are recorded, and the rendering is reproducible from
/// them.
///
/// Carrying the parts rather than the rendered text is deliberate: a surface
/// that wants to show which provider said what has it, and nothing is lost
/// because the message is reproducible by the joining rule.
#[tokio::test]
async fn the_parts_are_recorded_and_the_message_is_reproducible_from_them() {
    let h = Harness::new("ctx-parts").await;
    h.context.register_static("time", "The date is 2026-08-24.");
    h.context
        .register_static("workspace", "The working directory is /srv/app.");

    h.engine.run_turn("hello").await.expect("turn");
    let events = h.engine.log().events();
    let snapshot = snapshots(&events)[0];
    let parts = parts_of(snapshot);

    assert_eq!(
        parts,
        [
            part("time", "The date is 2026-08-24."),
            part("workspace", "The working directory is /srv/app."),
        ]
    );
    let derived = derive_messages(&events)
        .into_iter()
        .find(|m| m.content.contains("The date is"))
        .expect("in history");
    assert_eq!(
        derived.content,
        render(&parts).expect("rendered"),
        "history is exactly what the joining rule makes of the recorded parts"
    );
}

/// TC-CTX-7: the joining rule is section 4.3's, not a second one.
///
/// Non-empty parts, joined with a blank line, in list order. Two joining rules
/// in one system would be one too many, and the one that drifts is the one
/// nobody is looking at.
#[test]
fn the_joining_rule_is_the_same_one_prompt_sections_use() {
    assert_eq!(
        render(&[part("a", "first"), part("b", "second")]),
        Some("first\n\nsecond".to_owned())
    );
    assert_eq!(
        render(&[part("a", "first"), part("gap", ""), part("b", "second")]),
        Some("first\n\nsecond".to_owned()),
        "an empty part leaves no gap behind it"
    );
    assert_eq!(render(&[part("a", ""), part("b", "")]), None);
    assert_eq!(render(&[]), None);
}

/// TC-CTX-8: registration order is the order the model reads.
///
/// Not sorted by name. A deployment that puts the workspace before the date
/// meant that, and a registry that sorted would quietly rewrite the paragraph.
#[tokio::test]
async fn registration_order_is_the_order_the_model_reads() {
    let h = Harness::new("ctx-order").await;
    h.context.register_static("zeta", "first registered");
    h.context.register_static("alpha", "second registered");

    h.engine.run_turn("hello").await.expect("turn");
    let events = h.engine.log().events();
    let names: Vec<String> = parts_of(snapshots(&events)[0])
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["zeta", "alpha"]);
}

/// TC-CTX-9: each provider is asked once per turn, not once per step.
///
/// It is a snapshot of the turn. Asking per step would let the recorded value
/// disagree with the message the model actually read, and would charge a
/// deployment's providers for every step of a long turn.
#[tokio::test]
async fn each_provider_is_asked_once_per_turn() {
    let asked = Arc::new(AtomicUsize::new(0));
    let h = Harness::new("ctx-once").await;
    h.context.register(Arc::new(Counting {
        name: "time".into(),
        text: "now".into(),
        asked: Arc::clone(&asked),
    }));

    // The mock adapter's turn runs several steps, which is the point.
    h.engine.run_turn("hello").await.expect("turn");
    let steps = h
        .engine
        .log()
        .events()
        .iter()
        .filter(|e| e.ty == topic::STEP_START)
        .count();

    assert!(steps > 1, "the fixture ran a multi-step turn: {steps}");
    assert_eq!(asked.load(Ordering::SeqCst), 1, "asked once, not per step");
}

/// TC-CTX-10: a provider with a bug contributes nothing and does not fail the
/// turn.
///
/// A runtime context is a decoration on the work, not the work. Failing a turn
/// because the clock could not be read would let an optional provider stop a
/// deployment, and the failure mode a person would then see - "the agent stops
/// working" - is far worse than a missing sentence.
#[tokio::test]
async fn a_panicking_provider_is_contained_and_the_turn_runs() {
    static QUIET: Once = Once::new();
    QUIET.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !info
                .to_string()
                .contains("deliberate context provider fault")
            {
                previous(info);
            }
        }));
    });

    struct Faulty;
    impl ContextProvider for Faulty {
        fn name(&self) -> &str {
            "faulty"
        }
        fn text(&self) -> String {
            panic!("deliberate context provider fault")
        }
    }
    let h = Harness::new("ctx-panic").await;
    h.context.register(Arc::new(Faulty));
    h.context.register_static("time", "The date is 2026-08-24.");

    h.engine
        .run_turn("hello")
        .await
        .expect("the turn still runs");

    let events = h.engine.log().events();
    let parts = parts_of(snapshots(&events)[0]);
    assert_eq!(
        parts,
        [part("faulty", ""), part("time", "The date is 2026-08-24."),],
        "the faulty provider contributes nothing and the rest still speak"
    );
}
