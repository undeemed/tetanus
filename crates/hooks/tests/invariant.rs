//! Conformance: what every journal's `hook/*` records satisfy.
//!
//! Feature under test: `tetanus_hooks::invariant::hook_stream_faults` — the
//! pairing, enclosure and field rules of the hook audit trail.
//!
//! Ported from upstream `packages/hooks/hook-protocol/tests/invariant.spec.ts`.
//! Case ids TC-HOOK-INV-1..11. The last two are this port's own.
//!
//! Upstream registers these rules as a plugin that refuses a bad append. This
//! workspace checks a journal that was written, which is the same choice
//! `upstream_session_invariants.rs` already records for the session store. The
//! rules are upstream's; only the moment of checking differs, so each of
//! upstream's `toThrow` cases becomes "this fault is reported".

use serde_json::{json, Value};
use tetanus_hooks::invariant::hook_stream_faults;
use tetanus_session::SessionEvent;

/// A journal record, built directly so a case can write a malformed one.
fn event(ty: &str, data: Value) -> SessionEvent {
    SessionEvent {
        ty: ty.to_owned(),
        seq: 0,
        time: 0,
        data,
        source_event_seqs: None,
    }
}

fn turn_start(turn: u64) -> SessionEvent {
    event("turn/start", json!({ "turn": turn }))
}

fn turn_end(turn: u64) -> SessionEvent {
    event(
        "turn/end",
        json!({"turn": turn, "reason": {"kind": "completed"}}),
    )
}

/// A well-formed invocation, with overrides merged over it.
fn invoked(overrides: Value) -> SessionEvent {
    let mut data = json!({
        "turn": 1,
        "point": "PreToolUse",
        "dialect": "claude-code",
        "handlerId": "hook-1",
    });
    merge(&mut data, overrides);
    event("hook/invoked", data)
}

/// A well-formed result, with overrides merged over it.
fn result(overrides: Value) -> SessionEvent {
    let mut data = json!({
        "turn": 1,
        "point": "PreToolUse",
        "handlerId": "hook-1",
        "decision": "pass",
        "durationMs": 3,
    });
    merge(&mut data, overrides);
    event("hook/result", data)
}

fn merge(into: &mut Value, from: Value) {
    let (Some(target), Some(source)) = (into.as_object_mut(), from.as_object()) else {
        return;
    };
    for (key, value) in source {
        target.insert(key.clone(), value.clone());
    }
}

/// The one fault a case expected, or a readable failure naming what it got.
fn only_fault(events: &[SessionEvent]) -> String {
    let faults = hook_stream_faults(events);
    assert_eq!(
        faults.len(),
        1,
        "expected exactly one fault, got {faults:?}"
    );
    faults.into_iter().next().unwrap_or_default()
}

/// TC-HOOK-INV-1: a well-formed trail has nothing to report, including a
/// handler that fired twice at one point and was answered twice.
#[test]
fn serial_and_repeated_invocations_pair_cleanly() {
    let events = [
        turn_start(1),
        invoked(json!({})),
        invoked(json!({})),
        event("step/start", json!({"turn": 1, "step": 1})),
        result(json!({})),
        result(json!({})),
        turn_end(1),
    ];
    assert_eq!(hook_stream_faults(&events), Vec::<String>::new());
}

/// TC-HOOK-INV-2: a pair spanning other records is still a pair.
#[test]
fn an_invocation_answered_later_in_the_turn_is_paired() {
    let events = [
        turn_start(1),
        invoked(json!({})),
        event("assistant/message", json!({"turn": 1})),
        result(json!({})),
        turn_end(1),
    ];
    assert!(hook_stream_faults(&events).is_empty());
}

/// TC-HOOK-INV-3: a hook record outside any turn is unenclosed.
#[test]
fn a_hook_record_outside_a_turn_is_a_fault() {
    assert_eq!(
        only_fault(&[invoked(json!({}))]),
        "hook/invoked appended outside any open turn"
    );
}

/// TC-HOOK-INV-4: a record naming another turn than the open one is a fault,
/// and names both so the mismatch is readable.
#[test]
fn a_record_naming_another_turn_is_a_fault() {
    assert_eq!(
        only_fault(&[turn_start(1), invoked(json!({"turn": 2}))]),
        "hook/invoked names turn 2 but open turn is 1"
    );
}

/// TC-HOOK-INV-5: a record after the turn closed is unenclosed again.
#[test]
fn a_record_after_the_turn_closed_is_unenclosed() {
    let events = [turn_start(1), turn_end(1), invoked(json!({}))];
    assert_eq!(
        only_fault(&events),
        "hook/invoked appended outside any open turn"
    );
}

/// TC-HOOK-INV-6: an invocation missing what identifies it cannot be answered.
#[test]
fn an_invocation_without_a_point_or_handler_is_a_fault() {
    for overrides in [json!({"point": ""}), json!({"handlerId": ""})] {
        assert_eq!(
            only_fault(&[turn_start(1), invoked(overrides.clone())]),
            "hook/invoked point and handlerId must be non-empty",
            "for {overrides}"
        );
    }
}

/// TC-HOOK-INV-7: a dialect nobody speaks is a fault, and the value is quoted
/// so a reader can see what was written.
#[test]
fn an_unknown_dialect_is_a_fault() {
    assert_eq!(
        only_fault(&[turn_start(1), invoked(json!({"dialect": "other"}))]),
        "hook/invoked carries unknown dialect \"other\""
    );
}

/// TC-HOOK-INV-8: a result answering nothing is a fault.
#[test]
fn a_result_with_no_invocation_is_a_fault() {
    assert_eq!(
        only_fault(&[turn_start(1), result(json!({}))]),
        "hook/result has no matching hook/invoked for \"hook-1\""
    );
}

/// TC-HOOK-INV-9: a result must answer the invocation at its *own* point. One
/// handler configured at two points is two different pairs.
#[test]
fn a_result_at_another_point_answers_nothing() {
    let events = [
        turn_start(1),
        invoked(json!({})),
        result(json!({"point": "Stop"})),
    ];
    assert_eq!(
        only_fault(&events),
        "hook/result has no matching hook/invoked for \"hook-1\""
    );
}

/// TC-HOOK-INV-10: a duration that is not a usable number is a fault, however
/// it is unusable.
///
/// This port's own in its breadth: upstream pins a negative duration, and the
/// absent and wrong-typed cases are the ones a producer is likelier to write.
/// All three leave the audit trail without the timing it promises, so all
/// three are the same fault.
#[test]
fn a_duration_that_is_not_a_usable_number_is_a_fault() {
    for overrides in [
        json!({"durationMs": -1}),
        json!({"durationMs": null}),
        json!({"durationMs": "3"}),
    ] {
        let events = [turn_start(1), invoked(json!({})), result(overrides.clone())];
        assert_eq!(
            hook_stream_faults(&events),
            ["hook/result durationMs must be a non-negative finite number"],
            "for {overrides}"
        );
    }
}

/// TC-HOOK-INV-11: every fault is reported, not the first.
///
/// This port's own. A producer being fixed wants the whole list; stopping at
/// the first turns one debugging session into several. Upstream's plugin
/// throws on the first because it is refusing an append, which is a different
/// job from describing a journal.
#[test]
fn every_fault_is_reported_not_only_the_first() {
    let events = [
        invoked(json!({"dialect": "other", "point": ""})),
        turn_start(1),
        result(json!({"durationMs": -1})),
    ];
    let faults = hook_stream_faults(&events);
    assert_eq!(
        faults,
        [
            "hook/invoked appended outside any open turn",
            "hook/invoked point and handlerId must be non-empty",
            "hook/invoked carries unknown dialect \"other\"",
            "hook/result has no matching hook/invoked for \"hook-1\"",
            "hook/result durationMs must be a non-negative finite number",
        ]
    );
}
