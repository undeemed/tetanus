//! The runtime context: what a turn tells the model about the world outside
//! the conversation.
//!
//! Some of what a model needs is not in the conversation and is not stable -
//! today's date, the working directory, the branch. A provider contributes one
//! named part, the engine gathers them once per turn, and the record of what
//! was gathered is `context/snapshot` on the journal
//! (`docs/interface-contract.md` section 4.4.8).
//!
//! **It is a user message and not a prompt section, and that is the whole
//! design.** A provider caches a prompt by its longest stable prefix; the
//! system prompt is the same on every turn of a session, so it caches, and a
//! sentence saying what time it is would invalidate that prefix on every
//! request of every session. The snapshot travels after the retained history
//! instead, where it costs a message and no cache.
//!
//! Upstream builds each of these as its own plugin on `agent/pre-step`
//! (`packages/context/time-context`, `packages/context/tmux-context`), each
//! appending its own user message. tetanus gathers them into one snapshot for
//! the reason section 4.4.8 gives: only the newest one derives to a message,
//! and "newest" has to be a single record for that rule to be decidable.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use tetanus_core::EffectHandle;

/// One provider's contribution to a turn's runtime context.
///
/// The parts are the record and the rendering is derived from them, which is
/// the same choice section 4.3 makes for prompt sections: a surface that wants
/// to show which provider said what has it, and the text the model read is
/// still reproducible by the joining rule below.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextPart {
    pub name: String,
    pub text: String,
}

/// Which turn a context is being gathered for. A provider is told, because a
/// reading is about a moment and the turn is how a reader finds that moment
/// again on the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextAt {
    pub turn: u64,
}

/// What one provider is: a name and something that produces text for a turn.
///
/// A provider returns a `String` rather than an `Option<String>` because "I
/// have nothing to say this time" and "I said nothing" are the same fact to
/// every reader of the record, and one spelling of it is enough. An empty part
/// is dropped from the rendering and a snapshot of nothing but empties is
/// never written.
type Provider = Arc<dyn Fn(&ContextAt) -> String + Send + Sync>;

/// Where a snapshot's text is joined, so a reader of the journal can reproduce
/// exactly what the model was shown.
///
/// The rule is section 4.3's rule for prompt sections, deliberately: the parts
/// whose text is non-empty, joined with a blank line, in the order the list
/// gives. Two joining rules would be one too many.
pub fn render(parts: &[ContextPart]) -> String {
    parts
        .iter()
        .map(|part| part.text.trim_end())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The registry of runtime-context providers a session runs.
///
/// Ordering is the deployment's and is settled here, before the snapshot is
/// written - which is why there is no priority field on the durable record.
/// An order on the wire would let two readers disagree about the text the
/// model actually saw.
#[derive(Default)]
pub struct ContextRegistry {
    providers: Arc<Mutex<BTreeMap<(i32, String), Provider>>>,
}

impl ContextRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one provider, at one order, under one name.
    ///
    /// The handle removes it, as every registration does. A name registered
    /// twice at the same order replaces the first, which is what makes a
    /// composition that boots twice idempotent rather than doubled.
    pub fn provider<F>(&self, name: impl Into<String>, order: i32, produce: F) -> EffectHandle
    where
        F: Fn(&ContextAt) -> String + Send + Sync + 'static,
    {
        let key = (order, name.into());
        self.providers
            .lock()
            .expect("context providers")
            .insert(key.clone(), Arc::new(produce));
        let held: Weak<Mutex<BTreeMap<(i32, String), Provider>>> = Arc::downgrade(&self.providers);
        EffectHandle::new(move || {
            if let Some(providers) = held.upgrade() {
                providers.lock().expect("context providers").remove(&key);
            }
        })
    }

    /// Gather every provider's part for one turn, in registration order.
    ///
    /// A provider that panics contributes an empty part instead of ending the
    /// turn; see the note at the call.
    ///
    /// The gathering happens once, at the start of the turn: section 4.4.8
    /// says a snapshot is a fact about when the turn began and not a promise
    /// about the future, so nothing re-reads it and a step that runs for ten
    /// minutes is working from the time it started with.
    pub fn snapshot(&self, at: &ContextAt) -> Vec<ContextPart> {
        let providers: Vec<((i32, String), Provider)> = self
            .providers
            .lock()
            .expect("context providers")
            .iter()
            .map(|(key, provider)| (key.clone(), Arc::clone(provider)))
            .collect();
        providers
            .into_iter()
            .map(|((_, name), produce)| {
                // A provider that panics contributes nothing, and the turn
                // runs. A runtime context is a decoration on the work, not the
                // work: letting a plugin that cannot read a clock, a branch or
                // an environment variable end the turn trades a missing
                // sentence for an agent that stops - and the deployment that
                // installed the provider is rarely the one holding the
                // conversation. `crates/turn/src/tools.rs` contains a tool's
                // classifier the same way and for the same reason.
                //
                // The fault is logged rather than swallowed, because a
                // provider that silently says nothing for ever looks exactly
                // like one nobody registered.
                let text = match std::panic::catch_unwind(AssertUnwindSafe(|| produce(at))) {
                    Ok(text) => text,
                    Err(payload) => {
                        let fault = crate::tools::panicked(payload);
                        tracing::error!(
                            provider = name,
                            %fault,
                            "a runtime-context provider panicked; it contributes nothing this turn"
                        );
                        String::new()
                    }
                };
                ContextPart { name, text }
            })
            .collect()
    }
}

/// Where the clock a context reads comes from.
///
/// A parameter rather than a call to the system clock, for the reason every
/// other seam in this crate takes one: a case that asserts what a model was
/// told about the time cannot do it against a clock that moves while the case
/// runs.
pub type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// The system clock, for a composition that has no reason to supply another.
pub fn system_clock() -> Clock {
    Arc::new(SystemTime::now)
}

/// The name the time provider registers under, and the name its part carries
/// on the journal.
pub const TIME_PART: &str = "time";

/// Where the time part sits when a composition registers the built-ins.
pub const TIME_ORDER: i32 = 0;

/// The clock reading a turn tells the model about.
///
/// Upstream's `time-context` plugin, restated against this seam. Two of its
/// decisions are kept and one is not, and the one that is not is worth saying
/// plainly: upstream renders in a display time zone, configured or the
/// process's, and falls back to a zone it derives from the browser. tetanus
/// reports UTC, because a display zone means a time-zone database and the
/// workspace has no such dependency - the same reason `crates/turn` matches
/// phrases where upstream matches regular expressions. A reading nobody can
/// misread is better than a local time this build cannot be sure it converted
/// correctly, so the zone is stated in the text rather than assumed.
///
/// What is kept: the reading is durable rather than derived at request time,
/// so a replay shows the model what it was actually told; and the turn it was
/// sampled for is named, because a transcript with a bare timestamp cannot say
/// which request it belonged to.
pub fn time_provider(clock: Clock) -> impl Fn(&ContextAt) -> String + Send + Sync + 'static {
    move |at: &ContextAt| {
        let now = clock();
        format!(
            "Time sampled while preparing turn {}: {}",
            at.turn,
            format_utc(&now)
        )
    }
}

/// An epoch instant as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Written out rather than taken from a date library: this is the only date
/// formatting in the workspace, the civil-from-days conversion is a dozen
/// lines with a published proof, and a dependency for it would have to be
/// carried by every crate that links this one.
///
/// An instant before the epoch cannot arise from `SystemTime::now`, and a
/// clock a case supplies is the only other source; it renders as the epoch
/// rather than panicking, because a context provider that kills a turn over a
/// clock reading is worse than one that is briefly wrong.
fn format_utc(at: &SystemTime) -> String {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rest / 3_600, (rest % 3_600) / 60, rest % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// The civil date a count of days since 1970-01-01 names.
///
/// Howard Hinnant's `civil_from_days`, which is the algorithm every date
/// library uses for this: shift the epoch to 0000-03-01 so a leap day falls at
/// the end of the cycle, then count 400-, 100- and 4-year eras.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boundaries a hand-written date conversion gets wrong: a leap day, a
    /// century that is not a leap year, and the epoch itself.
    #[test]
    fn the_civil_date_holds_at_the_awkward_days() {
        let at = |secs: u64| format_utc(&(UNIX_EPOCH + std::time::Duration::from_secs(secs)));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z", "a leap day");
        assert_eq!(at(4_107_542_400), "2100-03-01T00:00:00Z", "not a leap year");
        assert_eq!(at(1_767_225_599), "2025-12-31T23:59:59Z");
    }
}
