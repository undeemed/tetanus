//! What a well-formed delegation lifecycle looks like.
//!
//! Providers are registered and removed; runs start and end. The parent's
//! bookkeeping — which children exist, which are still owed an answer — is
//! only as good as that stream being well formed, and a run that starts twice
//! or ends without starting corrupts it silently rather than loudly.
//!
//! The rules:
//!
//! - A provider has a name, and is not registered twice under it.
//! - A provider is not removed unless it was registered.
//! - A run names its provider, itself, and the child it started.
//! - A run id is not reused while its run is open.
//! - A run ends only if it started.
//! - A run's ending describes the same run as its beginning.
//!
//! That last one is the subtle one. `subagent/end` repeats the run's identity
//! rather than only its id, and if the repeat disagrees, one of the two events
//! is about a different run — which means a parent may be crediting an answer
//! to the wrong child.
//!
//! # A fold, not an append-time validator
//!
//! Upstream registers this as a cordis companion plugin that refuses a bad
//! dispatch as it happens. This workspace has no invariant registry — the
//! choice `crates/turn/tests/upstream_session_invariants.rs` records for the
//! session store, and the one `crates/hooks/src/invariant.rs` follows — so the
//! rules fold over a recorded stream and each of upstream's throws becomes a
//! reported fault.
//!
//! One consequence is visible in the rules above: upstream checks the identity
//! match even when no start was found, which in its own code cannot be reached
//! because the missing-start failure throws first. A fold that reports
//! everything must not report a divergence from a beginning that does not
//! exist, so the identity check is skipped in that case.
//!
//! Parity: upstream `packages/subagent/subagent/src/invariant.ts`, pinned by
//! its `invariant.spec.ts`.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// Who a run belongs to, repeated on both of its lifecycle events.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunIdentity {
    /// The provider that started it.
    pub provider: String,
    /// This run, distinct from every other open run.
    pub run_id: String,
    /// The child session it started.
    pub child_id: String,
    /// Whether the child runs in this process.
    pub local: bool,
}

/// One thing that happened to the delegation registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// A provider became available.
    ProviderAdded {
        /// Its name.
        name: String,
    },
    /// A provider went away.
    ProviderRemoved {
        /// Its name.
        name: String,
    },
    /// A run began.
    RunStarted(RunIdentity),
    /// A run finished.
    RunEnded(RunIdentity),
}

/// Every way a recorded lifecycle is malformed, in the order found.
///
/// All of them, not the first: a backend being fixed wants the whole list.
pub fn subagent_lifecycle_faults(events: &[LifecycleEvent]) -> Vec<String> {
    let mut faults = Vec::new();
    let mut providers: BTreeSet<&str> = BTreeSet::new();
    let mut open: BTreeMap<&str, &RunIdentity> = BTreeMap::new();

    for event in events {
        match event {
            LifecycleEvent::ProviderAdded { name } => {
                check_added(name, &mut providers, &mut faults);
            }
            LifecycleEvent::ProviderRemoved { name } => {
                check_removed(name, &mut providers, &mut faults);
            }
            LifecycleEvent::RunStarted(run) => check_started(run, &mut open, &mut faults),
            LifecycleEvent::RunEnded(end) => check_ended(end, &mut open, &mut faults),
        }
    }

    faults
}

/// A provider has a name, and is not registered twice under it.
fn check_added<'a>(name: &'a str, providers: &mut BTreeSet<&'a str>, faults: &mut Vec<String>) {
    if name.is_empty() {
        faults.push("subagent provider names must be non-empty".to_owned());
    }
    if !providers.insert(name) {
        faults.push(format!("subagent/provider-added repeated \"{name}\""));
    }
}

/// A provider is not removed unless it was registered.
fn check_removed(name: &str, providers: &mut BTreeSet<&str>, faults: &mut Vec<String>) {
    if !providers.remove(name) {
        faults.push(format!(
            "subagent/provider-removed names unknown provider \"{name}\""
        ));
    }
}

/// A run names its parts, and does not reuse the id of a run still open.
///
/// Deliberately not a provider-availability check: a one-shot run may outlive
/// the removal of the provider that started it, and a resumed run records the
/// provider it began under without going through it again.
fn check_started<'a>(
    run: &'a RunIdentity,
    open: &mut BTreeMap<&'a str, &'a RunIdentity>,
    faults: &mut Vec<String>,
) {
    if run.provider.is_empty() || run.run_id.is_empty() || run.child_id.is_empty() {
        faults.push("subagent/start provider, runId, and child id must be non-empty".to_owned());
    }
    if open.insert(&run.run_id, run).is_some() {
        faults.push(format!("subagent/start repeated run id \"{}\"", run.run_id));
    }
}

/// A run ends only if it started, and its ending describes the same run.
fn check_ended(
    end: &RunIdentity,
    open: &mut BTreeMap<&str, &RunIdentity>,
    faults: &mut Vec<String>,
) {
    match open.remove(end.run_id.as_str()) {
        Some(start) if start != end => faults.push(format!(
            "subagent/end identity diverges from subagent/start for run \"{}\"",
            end.run_id
        )),
        Some(_) => {}
        // No beginning to diverge from, so only the one fault.
        None => faults.push(format!(
            "subagent/end has no matching subagent/start for run \"{}\"",
            end.run_id
        )),
    }
}
