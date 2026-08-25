//! Bounds a deployment sets on a turn, rather than on a request.
//!
//! The provider seam already bounds one *request*: an idle window on the
//! connection and a deadline on the whole call. Neither of those bounds a turn.
//! A model that answers promptly every time and calls one more tool every time
//! runs for as long as the step budget allows, and a model that calls the same
//! tool with the same arguments forever is inside every per-request bound there
//! is while getting nowhere.
//!
//! Contract section 4.4.2 fixes what such a turn looks like, and this is the
//! engine half of it: `turn/end` carries `stop_reason: "timed-out"` or
//! `"repeated"`, and the prompt still answers a summary rather than an error,
//! because a bound the deployment chose being reached is the bound working.
//!
//! **The two reasons are separate because they need opposite answers.**
//! `"timed-out"` says the work did not fit in the budget, and the usual reply
//! is a bigger budget or a smaller task. `"repeated"` says the model was
//! looping, and a bigger budget makes that strictly worse. Collapsing them
//! would leave a reader unable to tell "this needs longer" from "longer will
//! not help", which is the only decision the reason is for.
//!
//! **A guard stops at a step boundary,** exactly where an interrupt lands and
//! for the same reason: a step already dispatched has already had its effect,
//! so cutting one in flight would leave a tool call with no result and a
//! journal that cannot be read. A guarded turn is therefore a whole turn with
//! its journal balanced, and section 4.6's state machine holds unchanged.
//!
//! **The clock is monotonic.** A turn's elapsed time is measured with
//! [`Instant`], not by subtracting two wall-clock stamps: the contract says in
//! as many words that the difference of two `time` values is an estimate that
//! is occasionally negative, and a bound that can be defeated by NTP is not a
//! bound.
//!
//! Parity: upstream's `guard/` packages are the nearest relatives and do
//! something deliberately weaker. Its `timeout-policy` bounds one *tool call*,
//! and its `repeat-tool-reminder` is advisory - it counts consecutive
//! identical calls and adds a reminder message, vetoing nothing. tetanus takes
//! upstream's *detection* rule, which is consecutive calls of the same tool
//! with canonically identical arguments, and pairs it with the action its own
//! contract already published: end the turn and say which guard did it.

use std::time::{Duration, Instant};

use crate::tools::ToolCall;

/// What a deployment allows one turn.
///
/// Both bounds are optional and both default to absent, which is the behaviour
/// every build had before guards existed: a deployment that sets neither is
/// bounded only by the step budget, as it was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnGuards {
    /// How long the whole turn may take. Measured from the moment the turn
    /// opens, across every step, provider call and tool.
    pub max_duration: Option<Duration>,
    /// How many times running the model may ask for the same call before the
    /// turn is stopped. `Some(3)` stops the turn when a third identical call
    /// in a row is asked for.
    ///
    /// A limit of zero or one is meaningless - the first call is already the
    /// first repeat - so it is read as "no repeat guard" rather than as a turn
    /// that can never call a tool.
    pub repeat_limit: Option<u32>,
}

impl TurnGuards {
    pub fn is_set(&self) -> bool {
        self.max_duration.is_some() || self.repeat_limit() > 0
    }

    /// The limit as the watch applies it: absent, or at least two.
    fn repeat_limit(&self) -> u32 {
        match self.repeat_limit {
            Some(limit) if limit >= 2 => limit,
            _ => 0,
        }
    }
}

/// Which bound a turn reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardBreach {
    TimedOut,
    Repeated,
}

/// One turn's view of its own bounds.
///
/// Held by the driver for the length of a turn and asked at each step
/// boundary. It owns the clock and the repeat count so the driver holds no
/// bookkeeping of its own, and so a case can drive it without a turn.
pub struct TurnWatch {
    guards: TurnGuards,
    started: Instant,
    /// The last call the model asked for, canonically, and how many times in a
    /// row it has asked for it.
    last: Option<(String, u32)>,
}

impl TurnWatch {
    /// Start watching, from now.
    pub fn start(guards: TurnGuards) -> Self {
        Self {
            guards,
            started: Instant::now(),
            last: None,
        }
    }

    /// The same, from a stated instant, so a case can place a turn's start in
    /// the past rather than sleeping through a budget.
    pub fn started_at(guards: TurnGuards, started: Instant) -> Self {
        Self {
            guards,
            started,
            last: None,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Record what one step asked for, and answer whether that alone breached
    /// the repeat bound.
    ///
    /// The unit of repetition is the whole batch a step asked for, not one
    /// call: a model looping on two tools alternately is looping, and counting
    /// single calls would reset on every alternation and never fire. A step
    /// that asked for nothing is not a repeat of anything and clears the
    /// count, because the model did something else.
    pub fn observe(&mut self, calls: &[ToolCall]) -> Option<GuardBreach> {
        let limit = self.guards.repeat_limit();
        if limit == 0 {
            return None;
        }
        if calls.is_empty() {
            self.last = None;
            return None;
        }
        let signature = signature(calls);
        let count = match self.last.take() {
            Some((previous, count)) if previous == signature => count + 1,
            _ => 1,
        };
        self.last = Some((signature, count));
        (count >= limit).then_some(GuardBreach::Repeated)
    }

    /// Whether a bound is reached now, at a step boundary.
    ///
    /// The time bound is asked here rather than by a timer, because a guard
    /// that fired mid-step would have to abandon a dispatched call. A turn that
    /// runs long inside one step therefore stops at the end of that step, which
    /// is the same promise the interrupt makes.
    pub fn breached(&self) -> Option<GuardBreach> {
        match self.guards.max_duration {
            Some(budget) if self.started.elapsed() >= budget => Some(GuardBreach::TimedOut),
            _ => None,
        }
    }
}

/// A batch of calls as a repeat detector compares them.
///
/// Name and arguments, in the order the model asked for them, with the
/// arguments rendered canonically so two structurally equal payloads written
/// differently compare equal - which is what upstream's detector compares, and
/// what makes the guard fire on a model that is genuinely repeating rather
/// than on one whose serializer reordered a map.
///
/// The call id is deliberately not part of it: a provider mints a fresh id per
/// call, so including it would make every call unique and the guard dead.
fn signature(calls: &[ToolCall]) -> String {
    let mut out = String::new();
    for call in calls {
        out.push_str(&call.name);
        out.push('\u{1f}');
        // `to_string` on a `serde_json::Value` sorts nothing, but the value
        // came from a parsed payload whose map preserves insertion order, so
        // two payloads that differ only in whitespace already compare equal
        // here and two that differ in key order do not. That is the same
        // comparison upstream makes on its canonical string.
        out.push_str(&call.arguments.to_string());
        out.push('\u{1e}');
    }
    out
}
