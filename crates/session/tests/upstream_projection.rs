//! Test Design Specification: session projections, ported.
//!
//! Feature under test: `tetanus_session::projection` - the unit contract, the
//! drive that folds committed events forward, the consistent read cut, and the
//! checkpoint that makes a value a cache rather than a recomputation.
//! Upstream pins the same machinery in
//! `packages/session/session-projection/tests/registry.spec.ts`; each case
//! names the upstream case it comes from.
//!
//! Approach: two units with deliberately different shapes - one that
//! accumulates a list and one that counts - so a case can tell "every unit was
//! driven" from "the first unit was driven". Events are built directly rather
//! than produced by a turn: a fold is a function of its input, and a real turn
//! would make the input harder to state, not more honest.
//!
//! What is not restated, and why. Upstream's registry is per-context and keys
//! its cells by session, because one Cordis context serves every session at
//! once; a tetanus session owns its own log, so `Projections` is per-session
//! and its "drives independently per session" case is structural rather than
//! asserted. Its merge-extensible `SessionProjectionMap` type table has no
//! Rust counterpart - a key is a string - and with it go the typed-registration
//! cases. Its zod `schema` field validates a view before it leaves the host;
//! tetanus has no schema layer on this seam, so "fails loud when a unit view
//! violates its own schema" has nothing to check against. Reference-counted
//! sharing of one key between registrants, and fiber-unload disposal, are
//! Cordis lifecycle rather than projection behaviour.
//!
//! One difference is a difference rather than an omission, and TC-PORT-PROJ-4
//! states it: upstream skips downstream work when `apply` returns the *same
//! reference*, so a unit that rebuilds an equal state still notifies there.
//! Comparing by value here reports strictly fewer changes and never one that
//! did not happen.
//!
//! Environmental needs: none. No case touches a filesystem, a network or an
//! API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use tetanus_session::projection::{
    Checkpoint, Projection, ProjectionError, Projections, EMPTY_LOG,
};
use tetanus_session::SessionEvent;

/// TC-PORT-PROJ-1: a registered unit is driven over committed events, and the
/// snapshot serves what it folded.
///
/// Upstream: "drives a registered unit over committed events and snapshots the
/// current value".
///
/// Input: three events, two of which the unit cares about.
/// Expected: the value is the fold of the two it cared about, and `as_of_seq`
/// is the last event's seq - not the last *interesting* event's, because the
/// watermark says how far the fold has been driven, not when it last changed.
#[test]
fn a_unit_is_driven_over_committed_events_and_snapshotted() {
    let p = Projections::new();
    p.register(marks()).expect("register");

    let changed = p.drive(&log(&[("mark", "a"), ("other", "x"), ("mark", "b")]));

    assert_eq!(changed, vec!["marks".to_string()]);
    let snapshot = p.snapshot();
    assert_eq!(snapshot.values["marks"], json!(["a", "b"]));
    assert_eq!(snapshot.as_of_seq, 2, "the watermark is how far, not when");
}

/// TC-PORT-PROJ-2: a unit registered after events flowed catches up.
///
/// Upstream: "builds the cell lazily from the full log for a unit registered
/// after events flowed".
///
/// This is what makes registration order stop mattering. A unit that served a
/// value folded only from the events after it arrived would give two
/// deployments different answers for the same journal, and the difference
/// would be invisible.
///
/// Input: a drive, then a second unit registered, then a drive over the same
/// whole log.
/// Expected: the late unit serves the fold of every event, identical to what
/// it would have served had it been registered first.
#[test]
fn a_unit_registered_late_folds_the_whole_log() {
    let events = log(&[("mark", "a"), ("mark", "b")]);

    let early = Projections::new();
    early.register(marks()).expect("register");
    early.drive(&events);

    let late = Projections::new();
    late.drive(&events);
    late.register(marks()).expect("register");
    late.drive(&events);

    assert_eq!(late.snapshot().values["marks"], json!(["a", "b"]));
    assert_eq!(late.snapshot(), early.snapshot());
}

/// TC-PORT-PROJ-3: an empty log serves the initial state, at seq -1.
///
/// Upstream: "serves init-derived state and asOfSeq -1 for an empty log".
///
/// `-1` is `SessionSubscribeResult.last_seq`'s spelling for the same fact, so
/// a reader holding both never has to translate between them.
///
/// Input: a registered unit, no events, then a drive over no events.
/// Expected: the init-derived value, `as_of_seq` of -1, and no change
/// reported - driving an empty log is not an event.
#[test]
fn an_empty_log_serves_the_initial_state_at_minus_one() {
    let p = Projections::new();
    p.register(marks()).expect("register");

    assert_eq!(p.snapshot().as_of_seq, EMPTY_LOG);
    assert_eq!(p.snapshot().values["marks"], json!([]));

    assert!(p.drive(&[]).is_empty(), "an empty drive changes nothing");
    assert_eq!(p.snapshot().as_of_seq, EMPTY_LOG);
}

/// TC-PORT-PROJ-4: only a unit whose value actually changed is reported.
///
/// Upstream: "notifies onChanged with the validated view and the causing seq,
/// and skips same-reference applies", and "runs every registered unit - a
/// changing unit notifies while a same-reference unit stays silent".
///
/// Upstream's test is reference identity (`Object.is`), so a unit that
/// rebuilds an equal state still notifies there. Rust has no cheap identity
/// for an owned value, so this compares the *view*, which reports strictly
/// fewer changes and never one that did not happen. The observable rule a
/// consumer depends on - "you are told when the value you can read is
/// different" - is the same, and stronger.
///
/// Input: both units registered; a drive carrying an event only one of them
/// folds; then a drive carrying an event neither folds.
/// Expected: the first drive reports only the interested unit; the second
/// reports nothing at all, though every unit ran.
#[test]
fn only_a_unit_whose_value_changed_is_reported() {
    let p = Projections::new();
    p.register(marks()).expect("register");
    p.register(counter()).expect("register");

    let changed = p.drive(&log(&[("mark", "a")]));
    assert_eq!(changed, vec!["marks".to_string()], "counter counts turns");

    let changed = p.drive(&log(&[("mark", "a"), ("other", "x")]));
    assert!(
        changed.is_empty(),
        "an event nobody folds is nobody's change: {changed:?}"
    );

    let changed = p.drive(&log(&[("mark", "a"), ("other", "x"), ("turn", "1")]));
    assert_eq!(changed, vec!["counter".to_string()]);
}

/// TC-PORT-PROJ-5: driving the same log twice folds nothing twice.
///
/// The watermark is what makes it safe to drive on every append without
/// tracking what was already delivered. A unit that re-folded the whole log
/// each time would double every accumulation, and a list is the shape where
/// that shows up rather than hides.
///
/// Input: one drive, then the identical log driven again, then the log with
/// one event appended.
/// Expected: the second drive changes nothing and leaves the value alone; the
/// third folds only the new event.
#[test]
fn driving_the_same_log_again_folds_nothing_twice() {
    let p = Projections::new();
    p.register(marks()).expect("register");
    let events = log(&[("mark", "a"), ("mark", "b")]);

    p.drive(&events);
    let after_first = p.snapshot();

    assert!(p.drive(&events).is_empty(), "nothing new to fold");
    assert_eq!(p.snapshot(), after_first, "and nothing folded twice");

    p.drive(&log(&[("mark", "a"), ("mark", "b"), ("mark", "c")]));
    assert_eq!(p.snapshot().values["marks"], json!(["a", "b", "c"]));
}

/// TC-PORT-PROJ-6: a checkpoint carries each unit's state, watermark and
/// version, and is detached from the live fold.
///
/// Upstream: "checkpoints every registered unit with its stateVersion and
/// per-cell watermark", and "checkpoint states are detached clones - mutating
/// them cannot corrupt the watermark cache".
///
/// Input: both units driven, then a checkpoint taken and its stored state
/// edited by the caller.
/// Expected: every registered key has a row carrying its own unit's version
/// and the watermark; editing the row leaves the live value untouched. A
/// checkpoint that shared state with the cache would let a consumer corrupt a
/// value nobody asked it to touch.
#[test]
fn a_checkpoint_carries_version_and_watermark_and_is_detached() {
    let p = Projections::new();
    p.register(marks()).expect("register");
    p.register(counter()).expect("register");
    p.drive(&log(&[("mark", "a"), ("turn", "1")]));

    let mut stored = p.checkpoint();
    assert_eq!(stored["marks"].ver, 1);
    assert_eq!(stored["counter"].ver, 7);
    assert_eq!(stored["marks"].seq, 1);
    assert_eq!(stored["marks"].val, json!(["a"]));

    stored.get_mut("marks").expect("row").val = json!(["tampered"]);
    assert_eq!(
        p.snapshot().values["marks"],
        json!(["a"]),
        "the live fold is not reachable through a checkpoint"
    );
}

/// TC-PORT-PROJ-7: restore adopts a usable row and folds only the tail past
/// it.
///
/// Upstream: "restore folds the tail past each usable row", and "restore over
/// a suffix folds only past each row watermark and serves an exact empty-tail
/// cut".
///
/// This is the whole point of a checkpoint: the events before the watermark
/// are not folded again.
///
/// Input: a checkpoint taken at seq 1, restored into a fresh registry against
/// a log that has grown by one event; and separately restored against exactly
/// the log it was taken from.
/// Expected: the grown log folds only the new event and lands on the same
/// value a full fold would give; the unchanged log folds nothing and reports
/// no change at all.
#[test]
fn restore_adopts_a_usable_row_and_folds_only_the_tail() {
    let taken = {
        let p = Projections::new();
        p.register(marks()).expect("register");
        p.drive(&log(&[("mark", "a"), ("mark", "b")]));
        p.checkpoint()
    };

    let grown = log(&[("mark", "a"), ("mark", "b"), ("mark", "c")]);
    let p = Projections::new();
    p.register(marks()).expect("register");
    let changed = p.restore(&taken, &grown);

    assert_eq!(changed, vec!["marks".to_string()]);
    assert_eq!(p.snapshot().values["marks"], json!(["a", "b", "c"]));
    assert_eq!(p.snapshot().as_of_seq, 2);

    // Restored against exactly the log it was taken from: an exact empty tail.
    let p = Projections::new();
    p.register(marks()).expect("register");
    let changed = p.restore(&taken, &log(&[("mark", "a"), ("mark", "b")]));
    assert!(
        changed.is_empty(),
        "nothing past the watermark: {changed:?}"
    );
    assert_eq!(p.snapshot().values["marks"], json!(["a", "b"]));
}

/// TC-PORT-PROJ-8: a row written by a different version of the unit is
/// discarded, and the log refolds from scratch.
///
/// Upstream: "refolds from init on version mismatch", and "refuses to share a
/// key across a stateVersion change".
///
/// Forward-applying a row whose state means something else is how one bad
/// checkpoint becomes a permanently wrong value that no number of new events
/// corrects. Refolding is always available because the log is the authority,
/// so discarding costs time and never correctness.
///
/// Input: a stored row claiming a version this unit does not have, over a log
/// the unit can fold.
/// Expected: the row is ignored, the value is the honest full fold, and it is
/// exactly what a registry with no checkpoint at all would serve.
#[test]
fn a_row_from_another_version_is_discarded_and_refolded() {
    let events = log(&[("mark", "a"), ("mark", "b")]);
    let stale = BTreeMap::from([(
        "marks".to_string(),
        Checkpoint {
            ver: 999,
            seq: 1,
            val: json!(["nonsense", "from", "an", "older", "unit"]),
        },
    )]);

    let p = Projections::new();
    p.register(marks()).expect("register");
    p.restore(&stale, &events);

    let fresh = Projections::new();
    fresh.register(marks()).expect("register");
    fresh.drive(&events);

    assert_eq!(p.snapshot().values["marks"], json!(["a", "b"]));
    assert_eq!(
        p.snapshot(),
        fresh.snapshot(),
        "a discarded row costs only time"
    );
}

/// TC-PORT-PROJ-9: a row claiming events the log does not have is discarded.
///
/// Upstream: "restore rejects a row claiming events past the supplied log end
/// (shrunk log implies re-read)".
///
/// A row folded further than the log in hand describes a history this caller
/// cannot show - the journal was truncated, replaced, or is a different one
/// under the same name. Folding the tail onto it would splice two histories
/// into one value.
///
/// Input: a row at seq 5 over a log whose last seq is 1; and the boundary case
/// of a row at exactly the log's last seq.
/// Expected: the over-reaching row is discarded and the log refolds; the row
/// that exactly matches the log end is kept, because reaching the end is not
/// passing it.
#[test]
fn a_row_claiming_more_than_the_log_holds_is_discarded() {
    let events = log(&[("mark", "a"), ("mark", "b")]);

    let over = BTreeMap::from([(
        "marks".to_string(),
        Checkpoint {
            ver: 1,
            seq: 5,
            val: json!(["from", "a", "longer", "log"]),
        },
    )]);
    let p = Projections::new();
    p.register(marks()).expect("register");
    p.restore(&over, &events);
    assert_eq!(
        p.snapshot().values["marks"],
        json!(["a", "b"]),
        "a row from a longer log is not a shortcut, it is a different history"
    );

    let exact = BTreeMap::from([(
        "marks".to_string(),
        Checkpoint {
            ver: 1,
            seq: 1,
            val: json!(["a", "b"]),
        },
    )]);
    let p = Projections::new();
    p.register(marks()).expect("register");
    assert!(
        p.restore(&exact, &events).is_empty(),
        "a row at exactly the log end is usable"
    );
    assert_eq!(p.snapshot().as_of_seq, 1);
}

/// TC-PORT-PROJ-10: a key with no stored row folds from init, beside keys that
/// have one.
///
/// Upstream: "restoreFloor anchors one below the lowest usable watermark and
/// at 0 for missing or mismatched rows", and "viewCheckpoint ... skips
/// mismatched keys".
///
/// tetanus has no separate floor to compute - `restore` folds each unit from
/// whatever it could adopt - so what is left to pin is the mixed case: one
/// unit shortcut and one refolded, in the same call, both correct.
///
/// Input: a checkpoint holding a row for one of two registered units.
/// Expected: both serve the full-fold value, and the snapshot is a single
/// consistent cut over the two.
#[test]
fn a_key_with_no_row_folds_from_init_beside_one_that_has() {
    let events = log(&[("mark", "a"), ("turn", "1"), ("mark", "b")]);
    let partial = BTreeMap::from([(
        "marks".to_string(),
        Checkpoint {
            ver: 1,
            seq: 0,
            val: json!(["a"]),
        },
    )]);

    let p = Projections::new();
    p.register(marks()).expect("register");
    p.register(counter()).expect("register");
    p.restore(&partial, &events);

    let snapshot = p.snapshot();
    assert_eq!(snapshot.values["marks"], json!(["a", "b"]));
    assert_eq!(snapshot.values["counter"], json!(1));
    assert_eq!(snapshot.as_of_seq, 2, "one cut over both");
}

/// TC-PORT-PROJ-11: a key is served by one unit, and removing it frees the
/// name.
///
/// Upstream: "shares one unit between registrants of the same key" and
/// "register() disposer removes the key (with its cells) and frees it for
/// re-registration". Upstream shares by reference counting because several
/// plugins may want the same projection; tetanus has one registry per session
/// and no plugin fan-out on this seam, so the same requirement reads as a
/// refusal: a second unit under a live key is a mistake, not a share.
///
/// Input: a duplicate registration, then a removal, then a re-registration.
/// Expected: the duplicate is refused by key; removal answers that it removed
/// something and frees the name; the re-registered unit folds from scratch,
/// with none of the removed unit's state left behind.
#[test]
fn a_key_is_served_once_and_removing_it_frees_the_name() {
    let p = Projections::new();
    p.register(marks()).expect("register");
    p.drive(&log(&[("mark", "a")]));

    match p.register(marks()) {
        Err(ProjectionError::DuplicateKey(key)) => assert_eq!(key, "marks"),
        other => panic!("expected a duplicate-key refusal, got {other:?}"),
    }

    assert!(p.remove("marks"), "it was there");
    assert!(!p.remove("marks"), "and now it is not");
    assert!(p.snapshot().values.is_empty());

    p.register(marks()).expect("the name is free again");
    assert_eq!(
        p.snapshot().values["marks"],
        json!([]),
        "a re-registered unit starts empty, not where the old one stopped"
    );
}

/// TC-PORT-PROJ-12: a checkpoint survives being stored as JSON.
///
/// The unit contract says state is plain JSON precisely so a row can be
/// persisted, and a checkpoint that only round-tripped in memory would meet
/// that requirement in name. Serializing it is what makes the precondition
/// real.
///
/// Input: a checkpoint serialized to a string and parsed back, then restored.
/// Expected: the parsed rows equal the taken ones, and restoring from them
/// serves the same value as restoring from the originals.
#[test]
fn a_checkpoint_survives_a_round_trip_through_json() {
    let events = log(&[("mark", "a"), ("mark", "b")]);
    let p = Projections::new();
    p.register(marks()).expect("register");
    p.drive(&events);
    let taken = p.checkpoint();

    let text = serde_json::to_string(&taken).expect("serialize");
    let parsed: BTreeMap<String, Checkpoint> = serde_json::from_str(&text).expect("parse");
    assert_eq!(parsed, taken);

    let restored = Projections::new();
    restored.register(marks()).expect("register");
    assert!(restored.restore(&parsed, &events).is_empty());
    assert_eq!(restored.snapshot(), p.snapshot());
}

// ---------------------------------------------------------------- fixtures

/// A unit that accumulates the payloads of every `mark` event.
///
/// A list is the shape that shows a double-fold: counting a thing twice can
/// look like counting two things, while a list says which.
struct Marks;

impl Projection for Marks {
    fn key(&self) -> &str {
        "marks"
    }
    fn state_version(&self) -> u32 {
        1
    }
    fn init(&self) -> Value {
        json!([])
    }
    fn apply(&self, mut state: Value, event: &SessionEvent) -> Value {
        if event.ty == "mark" {
            if let Some(marks) = state.as_array_mut() {
                marks.push(event.data["at"].clone());
            }
        }
        state
    }
    fn view(&self, state: &Value) -> Value {
        state.clone()
    }
}

/// A unit that counts `turn` events, so a case can tell one unit's drive from
/// another's.
struct Counter;

impl Projection for Counter {
    fn key(&self) -> &str {
        "counter"
    }
    fn state_version(&self) -> u32 {
        7
    }
    fn init(&self) -> Value {
        json!({ "turns": 0 })
    }
    fn apply(&self, mut state: Value, event: &SessionEvent) -> Value {
        if event.ty == "turn" {
            let seen = state["turns"].as_u64().unwrap_or_default() + 1;
            state["turns"] = json!(seen);
        }
        state
    }
    /// Serves a number where it holds an object, so a case that passed by
    /// comparing raw state rather than the view would not pass here.
    fn view(&self, state: &Value) -> Value {
        state["turns"].clone()
    }
}

fn marks() -> Arc<dyn Projection> {
    Arc::new(Marks)
}

fn counter() -> Arc<dyn Projection> {
    Arc::new(Counter)
}

/// A log of `(type, payload)` pairs, seq'd from zero as a real journal is.
fn log(entries: &[(&str, &str)]) -> Vec<SessionEvent> {
    entries
        .iter()
        .enumerate()
        .map(|(index, (ty, at))| SessionEvent {
            ty: (*ty).to_string(),
            seq: index as u64,
            time: index as u64,
            data: json!({ "at": at }),
            source_event_seqs: None,
        })
        .collect()
}
