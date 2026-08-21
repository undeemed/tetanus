//! How long a delegated child has actually been working.
//!
//! A parent showing "researcher — 42s" needs a number that survives a resume,
//! and the journal is the only thing that does. This is a
//! [`Projection`](tetanus_session::projection::Projection): a fold over the
//! child's log producing settled time plus, when a turn is open, the window it
//! has been open for.
//!
//! # The descriptor is the origin
//!
//! A child's journal can begin with inherited history — a fork seed, a parent's
//! transcript — and the turns in that prefix are the *parent's* work. The
//! `subagent/descriptor` record is where the child begins, and reaching it
//! resets settled time to zero. Without that reset a forked child would report
//! its parent's elapsed time as its own from the moment it started.
//!
//! The reset keeps one thing: a turn already open when the descriptor arrives
//! stays open, timed from its original start. That turn is the one that created
//! the child, so it is the child's first turn and not part of the prefix.
//!
//! # Time from a journal is not monotonic
//!
//! Timestamps are wall-clock, written across processes and possibly across a
//! clock adjustment. A turn can therefore appear to end before it began. Such a
//! turn contributes **zero**, never a negative: a duration that went backwards
//! is unknowable, and subtracting it would make a running total shrink and a
//! reader distrust every number on the page.
//!
//! Parity: upstream `packages/subagent/subagent/src/projection.ts`
//! (`subagentTimingProjectionDefinition`), pinned by its
//! `timing-projection.spec.ts`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tetanus_session::projection::Projection;
use tetanus_session::SessionEvent;

/// The record marking where a child's own history begins.
const DESCRIPTOR: &str = "subagent/descriptor";

/// A turn that has not closed yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTurn {
    /// When it started.
    pub since: u64,
    /// The latest moment it is known to have still been running.
    pub through: u64,
}

/// What the fold carries between events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TimingState {
    /// Whether the child's own history has begun.
    descriptor_seen: bool,
    /// Completed child turns, summed.
    settled_ms: u64,
    /// The turn in progress, if one is.
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<OpenTurn>,
    /// A turn opened before the descriptor. Held only so its close can be
    /// recognised and discarded; it is never counted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_turn_start: Option<u64>,
}

/// The timing unit.
#[derive(Debug, Clone, Copy, Default)]
pub struct SubagentTiming;

impl SubagentTiming {
    /// The fold, over typed state.
    fn step(mut state: TimingState, event: &SessionEvent) -> TimingState {
        match event.ty.as_str() {
            "turn/start" => {
                if state.descriptor_seen {
                    state.active = Some(OpenTurn {
                        since: event.time,
                        through: event.time,
                    });
                } else {
                    state.pending_turn_start = Some(event.time);
                }
                state
            }
            // The child's history starts here, so everything before it was
            // somebody else's. A turn already open is kept: it is the turn
            // that created this child.
            DESCRIPTOR => {
                let since = state
                    .active
                    .map(|open| open.since)
                    .or(state.pending_turn_start);
                TimingState {
                    descriptor_seen: true,
                    settled_ms: 0,
                    active: since.map(|since| OpenTurn {
                        since,
                        through: event.time,
                    }),
                    pending_turn_start: None,
                }
            }
            "turn/end" => {
                if !state.descriptor_seen {
                    // A turn from the inherited prefix closing. Forgotten, not
                    // counted.
                    state.pending_turn_start = None;
                    return state;
                }
                let Some(open) = state.active.take() else {
                    return state;
                };
                // Saturating, not wrapping: see the module note on clocks.
                state.settled_ms = state
                    .settled_ms
                    .saturating_add(event.time.saturating_sub(open.since));
                state
            }
            // Any other record is evidence the turn was still running then.
            _ => {
                if let Some(open) = state.active.as_mut() {
                    open.through = event.time;
                }
                state
            }
        }
    }
}

impl Projection for SubagentTiming {
    fn key(&self) -> &str {
        "subagentTiming"
    }

    fn state_version(&self) -> u32 {
        2
    }

    fn init(&self) -> Value {
        serde_json::to_value(TimingState::default()).unwrap_or(Value::Null)
    }

    fn apply(&self, state: Value, event: &SessionEvent) -> Value {
        // A state this unit cannot read is a checkpoint from another shape.
        // Folding on from the empty state is the conservative answer, and the
        // registry's own version check is what normally prevents it.
        let current: TimingState = serde_json::from_value(state).unwrap_or_default();
        serde_json::to_value(Self::step(current, event)).unwrap_or(Value::Null)
    }

    fn view(&self, state: &Value) -> Value {
        let current: TimingState = serde_json::from_value(state.clone()).unwrap_or_default();
        let mut view = json!({ "settledMs": current.settled_ms });
        if let (Some(open), Some(object)) = (current.active, view.as_object_mut()) {
            object.insert(
                "active".into(),
                json!({"since": open.since, "through": open.through}),
            );
        }
        view
    }
}
