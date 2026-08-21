//! Conformance: how long a delegated child has been working.
//!
//! Feature under test: `tetanus_subagent::timing::SubagentTiming` — the
//! projection that turns a child's journal into settled time plus an open
//! window.
//!
//! Ported from upstream
//! `packages/subagent/subagent/tests/timing-projection.spec.ts`.
//! Case ids TC-SUB-TIME-1..10. The last four are this port's own.

use serde_json::json;
use tetanus_session::projection::Projection;
use tetanus_session::SessionEvent;
use tetanus_subagent::timing::SubagentTiming;

fn event(ty: &str, seq: u64, time: u64) -> SessionEvent {
    SessionEvent {
        ty: ty.to_owned(),
        seq,
        time,
        data: json!({}),
        source_event_seqs: None,
    }
}

/// Fold a whole journal and read the view.
fn fold(events: &[SessionEvent]) -> serde_json::Value {
    let unit = SubagentTiming;
    let mut state = unit.init();
    for event in events {
        state = unit.apply(state, event);
    }
    unit.view(&state)
}

/// TC-SUB-TIME-1: an empty journal has settled no time.
#[test]
fn an_empty_journal_has_settled_nothing() {
    assert_eq!(fold(&[]), json!({"settledMs": 0}));
}

/// TC-SUB-TIME-2: the descriptor resets inherited time, and later completed
/// turns sum.
///
/// The first turn belongs to the parent's prefix. The second is the child's,
/// timed from its own start; the third is the child's too.
#[test]
fn the_descriptor_resets_inherited_time_and_later_turns_sum() {
    let journal = [
        event("turn/start", 0, 100),
        event(DESCRIPTOR, 1, 110),
        event("turn/end", 2, 300),
        event("turn/start", 3, 1_000),
        event(DESCRIPTOR, 4, 1_100),
        event("turn/end", 5, 4_100),
        event("turn/start", 6, 10_000),
        event("turn/end", 7, 12_000),
    ];
    assert_eq!(fold(&journal), json!({"settledMs": 5_100}));
}

/// TC-SUB-TIME-3: an open turn is exposed, and a reversed boundary never
/// subtracts.
///
/// The first turn appears to end before it began, which a clock adjustment can
/// produce. It contributes zero rather than going backwards.
#[test]
fn an_open_turn_is_exposed_and_reversed_time_never_subtracts() {
    let journal = [
        event("turn/start", 0, 1_000),
        event(DESCRIPTOR, 1, 1_100),
        event("turn/end", 2, 900),
        event("turn/start", 3, 2_000),
        event("assistant/chunk", 4, 2_500),
    ];
    assert_eq!(
        fold(&journal),
        json!({"settledMs": 0, "active": {"since": 2_000, "through": 2_500}})
    );
}

/// TC-SUB-TIME-4: turns completed before the descriptor are not the child's.
#[test]
fn a_turn_completed_before_the_descriptor_is_not_counted() {
    let journal = [
        event("turn/start", 0, 100),
        event("turn/end", 1, 200),
        event(DESCRIPTOR, 2, 300),
    ];
    assert_eq!(fold(&journal), json!({"settledMs": 0}));
}

/// TC-SUB-TIME-5: records that say nothing about timing leave the state alone.
#[test]
fn records_before_the_descriptor_leave_the_state_alone() {
    let unit = SubagentTiming;
    let initial = unit.init();
    assert_eq!(
        unit.apply(initial.clone(), &event("assistant/chunk", 0, 1)),
        initial
    );
    assert_eq!(
        unit.apply(initial.clone(), &event("turn/end", 1, 2)),
        initial
    );

    let after = unit.apply(initial, &event(DESCRIPTOR, 2, 3));
    assert_eq!(
        unit.apply(after.clone(), &event("turn/end", 3, 4)),
        after,
        "a close with nothing open changes nothing"
    );
}

/// TC-SUB-TIME-6: a turn open when the descriptor arrives is kept, and timed
/// from its original start.
///
/// This port's own. That turn is the one that created the child, so it is the
/// child's first turn rather than part of the inherited prefix — and its start
/// predates the descriptor, which is the only reason the fold has to carry a
/// pending start at all.
#[test]
fn a_turn_open_at_the_descriptor_is_timed_from_its_own_start() {
    let journal = [
        event("turn/start", 0, 1_000),
        event(DESCRIPTOR, 1, 1_500),
        event("turn/end", 2, 3_000),
    ];
    assert_eq!(
        fold(&journal),
        json!({"settledMs": 2_000}),
        "timed from 1000, not from the descriptor at 1500"
    );
}

/// TC-SUB-TIME-7: the open window extends with every later record, and only
/// while a turn is open.
///
/// This port's own. `through` is what lets a reader show a live duration
/// between turn boundaries; if it did not advance, a long turn would look
/// frozen at its start.
#[test]
fn the_open_window_advances_only_while_a_turn_is_open() {
    let journal = [
        event(DESCRIPTOR, 0, 100),
        event("turn/start", 1, 200),
        event("assistant/chunk", 2, 300),
        event("tool/call", 3, 900),
    ];
    assert_eq!(
        fold(&journal),
        json!({"settledMs": 0, "active": {"since": 200, "through": 900}})
    );

    // After the turn closes, a later record does not reopen a window.
    let closed = [
        event(DESCRIPTOR, 0, 100),
        event("turn/start", 1, 200),
        event("turn/end", 2, 400),
        event("assistant/chunk", 3, 900),
    ];
    assert_eq!(fold(&closed), json!({"settledMs": 200}));
}

/// TC-SUB-TIME-8: a second descriptor resets again.
///
/// This port's own. A journal reaching two descriptors is a child re-seeded
/// from a new origin, and the later one is authoritative — the same reason the
/// first reset exists. TC-SUB-TIME-2 relies on this without stating it.
#[test]
fn a_later_descriptor_resets_again() {
    let journal = [
        event(DESCRIPTOR, 0, 100),
        event("turn/start", 1, 200),
        event("turn/end", 2, 1_200),
        event(DESCRIPTOR, 3, 1_300),
    ];
    assert_eq!(
        fold(&journal),
        json!({"settledMs": 0}),
        "the second origin discards what the first had settled"
    );
}

/// TC-SUB-TIME-9: the state survives a round trip, so a checkpoint is usable.
///
/// This port's own, and it is what makes this a projection rather than a
/// function. A state that did not deserialize back into itself would make a
/// persisted checkpoint fold on from the wrong value — the exact failure the
/// projection seam's version field exists to prevent, reached by a different
/// route.
#[test]
fn the_state_survives_being_persisted_and_reloaded() {
    let unit = SubagentTiming;
    let mut state = unit.init();
    for e in [
        event(DESCRIPTOR, 0, 100),
        event("turn/start", 1, 200),
        event("turn/end", 2, 700),
        event("turn/start", 3, 800),
    ] {
        state = unit.apply(state, &e);
    }

    let round_tripped: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&state).expect("serialize"))
            .expect("deserialize");
    assert_eq!(round_tripped, state, "the state is its own serialization");

    // Folding on from the reloaded state gives the same answer as never
    // having stopped.
    let resumed = unit.apply(round_tripped, &event("turn/end", 4, 1_000));
    let straight = unit.apply(state, &event("turn/end", 4, 1_000));
    assert_eq!(unit.view(&resumed), unit.view(&straight));
    assert_eq!(unit.view(&resumed), json!({"settledMs": 700}));
}

/// TC-SUB-TIME-10: a total that would overflow saturates.
///
/// This port's own. Times come off a journal that another process wrote, so a
/// corrupt or adversarial timestamp is reachable; a wrapped total would report
/// a child that had run for eons as one that had barely started.
#[test]
fn an_absurd_timestamp_saturates_rather_than_wrapping() {
    let journal = [
        event(DESCRIPTOR, 0, 0),
        event("turn/start", 1, 0),
        event("turn/end", 2, u64::MAX),
        event("turn/start", 3, 0),
        event("turn/end", 4, u64::MAX),
    ];
    assert_eq!(fold(&journal), json!({"settledMs": u64::MAX}));
}

/// The record marking where a child's own history begins.
const DESCRIPTOR: &str = "subagent/descriptor";
