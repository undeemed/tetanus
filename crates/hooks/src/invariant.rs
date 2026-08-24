//! What every journal's `hook/*` records satisfy.
//!
//! The two hook events are only useful as an audit trail if the trail is
//! well formed: each result answers an invocation that happened, both sit
//! inside the turn they name, and the fields a reader relies on are actually
//! there. This module states those rules once, as a fold over a journal.
//!
//! # A fold, not an append-time validator
//!
//! Upstream registers this as a cordis companion plugin that refuses a bad
//! append as it happens. This workspace has no such registry — the same choice
//! `crates/turn/tests/upstream_session_invariants.rs` already records for the
//! session store's own rules — so the claim here is about what a writer
//! produced, checked over the journal it wrote. The rules are upstream's; only
//! the moment of checking differs.
//!
//! That difference has one consequence worth stating: a fold cannot stop a bad
//! record being written, so this is a conformance check on producers rather
//! than a guard against them. It is used by the suite that drives real hooks,
//! and it is what an adapter's own tests fold their journals through.
//!
//! Parity: upstream `packages/hooks/hook-protocol/src/invariant.ts`, pinned by
//! its `invariant.spec.ts`.

use std::collections::BTreeMap;

use serde_json::Value;
use tetanus_session::SessionEvent;

/// The two event types this module is about.
const INVOKED: &str = "hook/invoked";
const RESULT: &str = "hook/result";

/// Every way a journal's hook records are malformed, in the order found.
///
/// An empty answer means the trail is well formed. Every fault is reported
/// rather than the first, because a producer being fixed wants the whole list.
pub fn hook_stream_faults(events: &[SessionEvent]) -> Vec<String> {
    let mut faults = Vec::new();
    let mut open_turn: Option<u64> = None;
    // How many invocations are still waiting for their result, per pair key.
    // A count rather than a set: the same handler may fire twice at one point
    // in one turn, and both are owed an answer.
    let mut pending: BTreeMap<String, u64> = BTreeMap::new();

    for event in events {
        match event.ty.as_str() {
            "turn/start" => {
                open_turn = event.data.get("turn").and_then(Value::as_u64);
                continue;
            }
            "turn/end" => {
                open_turn = None;
                continue;
            }
            INVOKED | RESULT => {}
            _ => continue,
        }

        check_enclosure(event, open_turn, &mut faults);

        if event.ty == INVOKED {
            check_invocation(event, &mut faults);
            *pending.entry(pair_key(event)).or_default() += 1;
        } else {
            check_result(event, &pending, &mut faults);
            if let Some(count) = pending.get_mut(&pair_key(event)) {
                *count -= 1;
                if *count == 0 {
                    pending.remove(&pair_key(event));
                }
            }
        }
    }

    faults
}

/// A hook record belongs to exactly one open turn.
fn check_enclosure(event: &SessionEvent, open_turn: Option<u64>, faults: &mut Vec<String>) {
    let Some(open) = open_turn else {
        faults.push(format!("{} appended outside any open turn", event.ty));
        return;
    };
    let named = event.data.get("turn").and_then(Value::as_u64);
    if named != Some(open) {
        faults.push(format!(
            "{} names turn {} but open turn is {open}",
            event.ty,
            named.map_or_else(|| "none".to_owned(), |t| t.to_string()),
        ));
    }
}

/// What an invocation must carry to be answerable later.
fn check_invocation(event: &SessionEvent, faults: &mut Vec<String>) {
    let point = field(event, "point");
    let handler = field(event, "handlerId");
    if point.is_empty() || handler.is_empty() {
        faults.push("hook/invoked point and handlerId must be non-empty".to_owned());
    }
    let dialect = field(event, "dialect");
    if dialect != "claude-code" && dialect != "codex" {
        faults.push(format!(
            "hook/invoked carries unknown dialect \"{dialect}\""
        ));
    }
}

/// A result answers an invocation that happened, and reports a usable duration.
fn check_result(event: &SessionEvent, pending: &BTreeMap<String, u64>, faults: &mut Vec<String>) {
    if pending.get(&pair_key(event)).copied().unwrap_or(0) == 0 {
        faults.push(format!(
            "hook/result has no matching hook/invoked for \"{}\"",
            field(event, "handlerId")
        ));
    }
    // Absent, negative, or not a number at all: all the same fault, because
    // each of them leaves the audit trail without the timing it promises.
    let usable = event
        .data
        .get("durationMs")
        .and_then(Value::as_f64)
        .is_some_and(|ms| ms.is_finite() && ms >= 0.0);
    if !usable {
        faults.push("hook/result durationMs must be a non-negative finite number".to_owned());
    }
}

/// What makes an invocation and its result the same pair.
///
/// The handler id alone is not enough: one handler can be configured at two
/// points, and a result must answer the invocation at its own point in its own
/// turn. Joined with a NUL, which cannot occur in any of the three parts, so
/// two different triples can never collide into one key.
fn pair_key(event: &SessionEvent) -> String {
    format!(
        "{}\0{}\0{}",
        event.data.get("turn").and_then(Value::as_u64).unwrap_or(0),
        field(event, "point"),
        field(event, "handlerId"),
    )
}

/// A string field, or the empty string when it is absent or another type.
/// A wrong type is as unusable as an absent one, and both fail the same rule.
fn field<'a>(event: &'a SessionEvent, key: &str) -> &'a str {
    event.data.get(key).and_then(Value::as_str).unwrap_or("")
}
