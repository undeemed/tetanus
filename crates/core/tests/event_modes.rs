//! Test Design Specification: dispatch-mode conformance for the typed bus.
//!
//! Feature under test: the four dsh dispatch modes (upstream
//! `docs/cordis-primer.md`, "Dispatch Modes"). One test case per mode, each
//! with an explicit expected result.

use std::sync::{Arc, Mutex};

use tetanus_core::events::{DispatchMode, Event, EventBus, Terminal};

type Trace = Arc<Mutex<Vec<String>>>;

fn trace() -> Trace {
    Arc::new(Mutex::new(Vec::new()))
}

fn record(t: &Trace, what: &str) {
    t.lock().unwrap().push(what.to_string());
}

fn seen(t: &Trace) -> Vec<String> {
    t.lock().unwrap().clone()
}

struct Observed {
    note: String,
}
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

struct Asked {
    question: String,
}
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

fn passthrough() -> Terminal<Wrapped> {
    Arc::new(|ev: &mut Wrapped| Box::pin(async move { ev.input.clone() }))
}

/// TC-BUS-EMIT-1: listeners run synchronously in registration order and their
/// return values are ignored. Expected trace: `["first:hi", "second:hi"]`.
#[test]
fn emit_runs_listeners_in_registration_order() {
    let bus = EventBus::new();
    let t = trace();

    let (a, b) = (t.clone(), t.clone());
    let _first = bus.on_emit::<Observed>(move |ev| record(&a, &format!("first:{}", ev.note)));
    let _second = bus.on_emit::<Observed>(move |ev| record(&b, &format!("second:{}", ev.note)));

    bus.emit(&Observed { note: "hi".into() });

    assert_eq!(seen(&t), vec!["first:hi", "second:hi"]);
}

/// TC-BUS-EMIT-2: a listener registration is a reversible effect. Dropping the
/// handle removes it. Expected: one listener before the drop, zero after, and
/// no trace entries from the dropped listener.
#[test]
fn dropping_the_handle_unwinds_the_registration() {
    let bus = EventBus::new();
    let t = trace();

    let a = t.clone();
    let handle = bus.on_emit::<Observed>(move |ev| record(&a, &ev.note));
    assert_eq!(bus.listener_count::<Observed>(), 1);

    drop(handle);
    assert_eq!(bus.listener_count::<Observed>(), 0);

    bus.emit(&Observed {
        note: "gone".into(),
    });
    assert_eq!(seen(&t), Vec::<String>::new());
}

/// TC-BUS-PARALLEL-1: every listener observes the event and the dispatch
/// resolves only once all have settled, including a listener that yields.
/// Expected: both listeners recorded, in completion order `["fast", "slow"]`.
#[tokio::test]
async fn parallel_awaits_every_listener() {
    let bus = EventBus::new();
    let t = trace();

    let slow = t.clone();
    let _a = bus.on_parallel::<FannedOut, _>(move |_ev| {
        let slow = slow.clone();
        Box::pin(async move {
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            record(&slow, "slow");
        })
    });
    let fast = t.clone();
    let _b = bus.on_parallel::<FannedOut, _>(move |_ev| {
        let fast = fast.clone();
        Box::pin(async move { record(&fast, "fast") })
    });

    bus.parallel(&FannedOut).await;

    assert_eq!(seen(&t), vec!["fast", "slow"]);
}

/// TC-BUS-SERIAL-1: listeners are awaited in order until one bails; the bail
/// value is the dispatch result and later listeners never run.
/// Expected: result `Some("answer-from-second")`, trace `["first", "second"]`.
#[tokio::test]
async fn serial_stops_at_the_first_bail() {
    let bus = EventBus::new();
    let t = trace();

    let a = t.clone();
    let _first = bus.on_serial::<Asked, _>(move |_ev| {
        let a = a.clone();
        Box::pin(async move {
            record(&a, "first");
            None
        })
    });
    let b = t.clone();
    let _second = bus.on_serial::<Asked, _>(move |ev| {
        let b = b.clone();
        let answer = format!("answer-from-second:{}", ev.question);
        Box::pin(async move {
            record(&b, "second");
            Some(answer)
        })
    });
    let c = t.clone();
    let _third = bus.on_serial::<Asked, _>(move |_ev| {
        let c = c.clone();
        Box::pin(async move {
            record(&c, "third");
            None
        })
    });

    let bail = bus
        .serial(&Asked {
            question: "q".into(),
        })
        .await;

    assert_eq!(bail.as_deref(), Some("answer-from-second:q"));
    assert_eq!(seen(&t), vec!["first", "second"]);
}

/// TC-BUS-SERIAL-2: with no listener that bails, the dispatch resolves to
/// `None`. Expected: `None`.
#[tokio::test]
async fn serial_without_a_bail_returns_none() {
    let bus = EventBus::new();
    let _l = bus.on_serial::<Asked, _>(|_ev| Box::pin(async move { None }));
    assert_eq!(
        bus.serial(&Asked {
            question: "q".into()
        })
        .await,
        None
    );
}

/// TC-BUS-WATERFALL-1: listeners wrap the terminal as around-middleware. Each
/// sees the value the inner chain returned and may wrap it.
/// Expected result `outer(inner(BUILT-IN))`, trace showing the enter/leave nesting.
#[tokio::test]
async fn waterfall_wraps_the_terminal() {
    let bus = EventBus::new();
    let t = trace();

    let a = t.clone();
    let _outer = bus.on_waterfall::<Wrapped, _>(move |ev, next| {
        let a = a.clone();
        Box::pin(async move {
            record(&a, "outer:enter");
            let inner = next.run(ev).await;
            record(&a, "outer:leave");
            format!("outer({inner})")
        })
    });
    let b = t.clone();
    let _inner = bus.on_waterfall::<Wrapped, _>(move |ev, next| {
        let b = b.clone();
        Box::pin(async move {
            record(&b, "inner:enter");
            let deeper = next.run(ev).await;
            record(&b, "inner:leave");
            format!("inner({deeper})")
        })
    });

    let mut ev = Wrapped {
        input: "BUILT-IN".into(),
    };
    let out = bus.waterfall(&mut ev, passthrough()).await;

    assert_eq!(out, "outer(inner(BUILT-IN))");
    assert_eq!(
        seen(&t),
        vec!["outer:enter", "inner:enter", "inner:leave", "outer:leave"]
    );
}

/// TC-BUS-WATERFALL-2: a listener that returns without calling `next()` vetoes
/// the rest of the chain, including the built-in terminal.
/// Expected result `"vetoed"`, trace `["outer:enter"]` only.
#[tokio::test]
async fn waterfall_listener_can_veto_the_chain() {
    let bus = EventBus::new();
    let t = trace();

    let a = t.clone();
    let _veto = bus.on_waterfall::<Wrapped, _>(move |_ev, _next| {
        let a = a.clone();
        Box::pin(async move {
            record(&a, "outer:enter");
            "vetoed".to_string()
        })
    });
    let b = t.clone();
    let _never = bus.on_waterfall::<Wrapped, _>(move |ev, next| {
        let b = b.clone();
        Box::pin(async move {
            record(&b, "never");
            next.run(ev).await
        })
    });

    let mut ev = Wrapped {
        input: "BUILT-IN".into(),
    };
    let out = bus.waterfall(&mut ev, passthrough()).await;

    assert_eq!(out, "vetoed");
    assert_eq!(seen(&t), vec!["outer:enter"]);
}

/// TC-BUS-WATERFALL-3: with no listeners the built-in terminal runs and its
/// value is the result. Expected: `"BUILT-IN"`.
#[tokio::test]
async fn waterfall_without_listeners_runs_the_terminal() {
    let bus = EventBus::new();
    let mut ev = Wrapped {
        input: "BUILT-IN".into(),
    };
    assert_eq!(bus.waterfall(&mut ev, passthrough()).await, "BUILT-IN");
}

/// TC-BUS-MODE-1: the declared mode is part of the contract. Registering a
/// listener through another mode is a wiring error.
/// Expected: panic naming the event topic.
#[test]
#[should_panic(expected = "test/wrapped")]
fn using_the_wrong_mode_panics() {
    let bus = EventBus::new();
    let _ = bus.on_emit::<Wrapped>(|_ev| {});
}
