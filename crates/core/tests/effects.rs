//! Test Design Specification: reversible effects and the scope that composes
//! them.
//!
//! Feature under test: `tetanus_core::EffectScope`, and `Context` as its first
//! consumer. Upstream builds the same composite out of a generator that yields
//! disposers (`packages/core/scope/tests/scope.spec.ts`); the cases that
//! restate an upstream one name it.
//!
//! Approach: each case records the order its undos ran in, because order is
//! the whole contract. `TC-EFFECT-4` provokes a panic inside an undo, so the
//! default panic hook prints a backtrace line during an otherwise passing run.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic escaping the case.

use std::sync::{Arc, Mutex};

use tetanus_core::{Context, EffectHandle, EffectScope};

type Trace = Arc<Mutex<Vec<String>>>;

/// TC-EFFECT-1: a scope unwinds newest effect first.
///
/// Input: three effects registered in order.
/// Expected: they undo in the reverse order. An effect registered later may
/// stand on an earlier one, so this is the only order that never tears down
/// something still in use.
#[test]
fn a_scope_unwinds_newest_first() {
    let trace = trace();
    let mut scope = EffectScope::new();
    for name in ["first", "second", "third"] {
        scope.on_unwind(record(&trace, name));
    }

    assert_eq!(scope.len(), 3);
    let faults = scope.unwind();

    assert!(faults.is_empty());
    assert_eq!(seen(&trace), vec!["third", "second", "first"]);
    assert!(scope.is_empty());
}

/// TC-EFFECT-2: unwinding twice undoes nothing twice.
///
/// Upstream: "shares quiescence across repeat and raw-disposer-first calls" -
/// a scope disposed twice settles once.
///
/// Input: one effect; `unwind`, then `unwind` again, then drop.
/// Expected: the undo ran exactly once. An owner that cannot tell whether it
/// already unwound must be free to say so again.
#[test]
fn unwinding_twice_undoes_nothing_twice() {
    let trace = trace();
    let mut scope = EffectScope::new();
    scope.on_unwind(record(&trace, "once"));

    scope.unwind();
    scope.unwind();
    drop(scope);

    assert_eq!(seen(&trace), vec!["once"]);
}

/// TC-EFFECT-3: a nested scope unwinds at its own place in the outer order.
///
/// Upstream: "exposes the exact raw disposer for ordered composite teardown",
/// which expects `['inner', 'scope', 'outer']`.
///
/// Input: an outer scope holding, in order, an "outer" undo, a nested scope
/// with its own undo, and an "inner" undo.
/// Expected: `inner`, then the nested scope, then `outer`. The nested scope is
/// one effect to its owner, not a special case in the walk.
#[test]
fn a_nested_scope_unwinds_at_its_place_in_the_outer_order() {
    let trace = trace();
    let mut outer = EffectScope::new();
    outer.on_unwind(record(&trace, "outer"));

    let mut nested = EffectScope::new();
    nested.on_unwind(record(&trace, "scope"));
    outer.keep(nested.into_handle());

    outer.on_unwind(record(&trace, "inner"));
    outer.unwind();

    assert_eq!(seen(&trace), vec!["inner", "scope", "outer"]);
}

/// TC-EFFECT-4: an undo that panics does not strand the effects behind it.
///
/// Input: three effects, where the middle one panics.
/// Expected: both other undos run, and the panic comes back as one reported
/// fault carrying its message. The effects queued behind a bad undo are exactly
/// the ones that leak if the unwind stops, so the unwind does not stop.
#[test]
fn an_undo_that_panics_does_not_strand_the_rest() {
    let trace = trace();
    let mut scope = EffectScope::new();
    scope.on_unwind(record(&trace, "first"));
    scope.on_unwind(|| panic!("the undo failed"));
    scope.on_unwind(record(&trace, "third"));

    let faults = scope.unwind();

    assert_eq!(seen(&trace), vec!["third", "first"]);
    assert_eq!(faults.len(), 1);
    assert!(
        faults[0].to_string().contains("the undo failed"),
        "{}",
        faults[0]
    );
}

/// TC-EFFECT-5: a forgotten scope unwinds nothing.
///
/// Expected: no undo runs, now or at drop. A registration that outlives its
/// registrant on purpose - a process-wide hook - is kept this way rather than
/// by leaking the handle somewhere.
#[test]
fn a_forgotten_scope_unwinds_nothing() {
    let trace = trace();
    let mut scope = EffectScope::new();
    scope.on_unwind(record(&trace, "kept"));

    scope.forget();

    assert!(seen(&trace).is_empty());
}

/// TC-EFFECT-6: dropping a context unwinds the registrations it kept.
///
/// Input: two handles kept by a `Context`, which is the scope's first consumer.
/// Expected: dropping the context undoes them newest first. A plugin's wiring
/// dies with the context it was mounted on, and in the order that wiring was
/// built.
#[test]
fn dropping_a_context_unwinds_what_it_kept() {
    let trace = trace();
    let mut ctx = Context::new();
    ctx.keep(EffectHandle::new(record(&trace, "service")));
    ctx.keep(EffectHandle::new(record(&trace, "listener")));

    drop(ctx);

    assert_eq!(seen(&trace), vec!["listener", "service"]);
}

fn trace() -> Trace {
    Arc::new(Mutex::new(Vec::new()))
}

fn record(trace: &Trace, what: &str) -> impl FnOnce() + Send + Sync + 'static {
    let trace = Arc::clone(trace);
    let what = what.to_string();
    move || trace.lock().unwrap().push(what)
}

fn seen(trace: &Trace) -> Vec<String> {
    trace.lock().unwrap().clone()
}
