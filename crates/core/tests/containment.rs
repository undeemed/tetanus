//! Test Design Specification: a panicking listener and the blast radius it is
//! allowed to have.
//!
//! Feature under test: `EventBus::emit` and `EventBus::parallel` contain a
//! listener that panics; `EventBus::serial` and `EventBus::waterfall` do not.
//! Upstream pins the same line in
//! `core/agent-loop/tests/contract-regressions.spec.ts` ("a throwing
//! `step/start` observer cannot change a successful turn", "a throwing
//! `session/event` listener on `turn/end` is contained"), with a throw where
//! Rust has a panic.
//!
//! Approach: register a panicking listener between two recording ones and read
//! back what ran. The value-producing modes are pinned with `#[should_panic]`,
//! because "it stays loud" is a claim about the caller, not about a log line.
//!
//! Note on output: a contained panic still runs the default panic hook, so
//! these cases print `thread panicked at ...` while passing. The expected
//! results below are about behaviour, not about a quiet stderr.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, an escaped panic, or a hang.

use std::sync::{Arc, Mutex};

use tetanus_core::events::{DispatchMode, Event, EventBus, Terminal};

type Trace = Arc<Mutex<Vec<String>>>;

struct Observed;
impl Event for Observed {
    const TOPIC: &'static str = "test/observed";
    const MODE: DispatchMode = DispatchMode::Emit;
    type Output = ();
}

struct FannedOut;
impl Event for FannedOut {
    const TOPIC: &'static str = "test/fanned-out";
    const MODE: DispatchMode = DispatchMode::Parallel;
    type Output = ();
}

struct Asked;
impl Event for Asked {
    const TOPIC: &'static str = "test/asked";
    const MODE: DispatchMode = DispatchMode::Serial;
    type Output = String;
}

struct Wrapped {
    input: String,
}
impl Event for Wrapped {
    const TOPIC: &'static str = "test/wrapped";
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Output = String;
}

fn trace() -> Trace {
    Arc::new(Mutex::new(Vec::new()))
}

fn record(t: &Trace, what: &str) {
    t.lock().expect("trace lock").push(what.to_string());
}

fn seen(t: &Trace) -> Vec<String> {
    t.lock().expect("trace lock").clone()
}

/// TC-PORT-CONTAIN-1: an emit observer that panics does not take its peers
/// with it.
///
/// Input: three observers in registration order, the middle one panicking.
/// Expected: the first and the third both ran, and `emit` returned normally to
/// the component that dispatched. A durable fact stays made whatever an
/// observer of it does.
#[test]
fn a_panicking_emit_observer_leaves_its_peers_alone() {
    let bus = EventBus::new();
    let t = trace();

    let (first, third) = (t.clone(), t.clone());
    let _a = bus.on_emit::<Observed>(move |_| record(&first, "first"));
    let _b = bus.on_emit::<Observed>(|_| panic!("this observer has a bug"));
    let _c = bus.on_emit::<Observed>(move |_| record(&third, "third"));

    bus.emit(&Observed);
    record(&t, "dispatch returned");

    assert_eq!(seen(&t), vec!["first", "third", "dispatch returned"]);
}

/// TC-PORT-CONTAIN-2: containment is per dispatch, not per bus.
///
/// Expected: a second dispatch runs the same three observers again, so a
/// panicking listener is neither removed nor left in a state that suppresses
/// the next event. Upstream's "cannot starve the loop or later turns".
#[test]
fn a_contained_panic_does_not_disarm_the_next_dispatch() {
    let bus = EventBus::new();
    let t = trace();

    let survivor = t.clone();
    let _a = bus.on_emit::<Observed>(|_| panic!("still buggy"));
    let _b = bus.on_emit::<Observed>(move |_| record(&survivor, "ran"));

    bus.emit(&Observed);
    bus.emit(&Observed);

    assert_eq!(seen(&t), vec!["ran", "ran"]);
    assert_eq!(bus.listener_count::<Observed>(), 2, "neither was dropped");
}

/// TC-PORT-CONTAIN-3: a parallel observer that panics is contained too, and
/// the dispatch still resolves.
///
/// Expected: both peers ran and `parallel` returned. "All listeners settled"
/// has to include the one that panicked, or a dispatch would hang on it.
#[tokio::test]
async fn a_panicking_parallel_observer_still_lets_the_dispatch_settle() {
    let bus = EventBus::new();
    let t = trace();

    let (first, third) = (t.clone(), t.clone());
    let _a = bus.on_parallel::<FannedOut, _>(move |_| {
        let first = first.clone();
        Box::pin(async move { record(&first, "first") })
    });
    let _b = bus.on_parallel::<FannedOut, _>(|_| Box::pin(async { panic!("bug in flight") }));
    let _c = bus.on_parallel::<FannedOut, _>(move |_| {
        let third = third.clone();
        Box::pin(async move { record(&third, "third") })
    });

    bus.parallel(&FannedOut).await;

    let ran = seen(&t);
    assert_eq!(ran.len(), 2, "{ran:?}");
    assert!(ran.contains(&"first".to_string()) && ran.contains(&"third".to_string()));
}

/// TC-PORT-CONTAIN-4: a serial listener that panics is loud.
///
/// Expected: the panic reaches the caller. A serial dispatch asks for a value
/// and acts on the answer, so swallowing the panic would answer "nobody
/// bailed" when the truth is that nobody was asked.
#[tokio::test]
#[should_panic(expected = "a listener with a bug")]
async fn a_panicking_serial_listener_stays_loud() {
    let bus = EventBus::new();
    let _a = bus.on_serial::<Asked, _>(|_| Box::pin(async { panic!("a listener with a bug") }));

    bus.serial(&Asked).await;
}

/// TC-PORT-CONTAIN-5: a waterfall listener that panics is loud.
///
/// Expected: the panic reaches the caller. The chain composes the dispatching
/// component's own behaviour, so a contained panic would silently run the
/// terminal without the middleware that was supposed to wrap it.
#[tokio::test]
#[should_panic(expected = "a middleware with a bug")]
async fn a_panicking_waterfall_listener_stays_loud() {
    let bus = EventBus::new();
    let _a = bus
        .on_waterfall::<Wrapped, _>(|_, _| Box::pin(async { panic!("a middleware with a bug") }));
    let terminal: Terminal<Wrapped> =
        Arc::new(|ev: &mut Wrapped| Box::pin(async move { ev.input.clone() }));

    let mut event = Wrapped {
        input: "unreachable".into(),
    };
    bus.waterfall(&mut event, terminal).await;
}
