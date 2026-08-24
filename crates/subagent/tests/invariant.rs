//! Conformance: what a well-formed delegation lifecycle looks like.
//!
//! Feature under test: `tetanus_subagent::invariant` — the provider-registry
//! and run-pairing rules the parent's bookkeeping depends on.
//!
//! Ported from upstream `packages/subagent/subagent/src/invariant.ts` and its
//! `invariant.spec.ts`. Case ids TC-SUB-INV-1..10. The last three are this
//! port's own.
//!
//! Upstream refuses a bad dispatch as it happens; this workspace has no
//! invariant registry, so each of its throws becomes a reported fault. Same
//! choice as `crates/hooks/src/invariant.rs` and the session store's own.

use tetanus_subagent::invariant::{subagent_lifecycle_faults, LifecycleEvent, RunIdentity};

fn added(name: &str) -> LifecycleEvent {
    LifecycleEvent::ProviderAdded {
        name: name.to_owned(),
    }
}

fn removed(name: &str) -> LifecycleEvent {
    LifecycleEvent::ProviderRemoved {
        name: name.to_owned(),
    }
}

fn run(run_id: &str) -> RunIdentity {
    RunIdentity {
        provider: "spawn".into(),
        run_id: run_id.to_owned(),
        child_id: "child-1".into(),
        local: true,
    }
}

fn started(run_id: &str) -> LifecycleEvent {
    LifecycleEvent::RunStarted(run(run_id))
}

fn ended(run_id: &str) -> LifecycleEvent {
    LifecycleEvent::RunEnded(run(run_id))
}

fn only_fault(events: &[LifecycleEvent]) -> String {
    let faults = subagent_lifecycle_faults(events);
    assert_eq!(faults.len(), 1, "expected one fault, got {faults:?}");
    faults.into_iter().next().unwrap_or_default()
}

/// TC-SUB-INV-1: an ordinary lifecycle has nothing to report.
#[test]
fn an_ordinary_lifecycle_is_well_formed() {
    let events = [
        added("spawn"),
        started("r-1"),
        ended("r-1"),
        started("r-2"),
        ended("r-2"),
        removed("spawn"),
    ];
    assert_eq!(subagent_lifecycle_faults(&events), Vec::<String>::new());
}

/// TC-SUB-INV-2: a provider needs a name.
#[test]
fn a_provider_without_a_name_is_a_fault() {
    assert_eq!(
        only_fault(&[added("")]),
        "subagent provider names must be non-empty"
    );
}

/// TC-SUB-INV-3: a provider is not registered twice.
#[test]
fn a_provider_registered_twice_is_a_fault() {
    assert_eq!(
        only_fault(&[added("spawn"), added("spawn")]),
        "subagent/provider-added repeated \"spawn\""
    );
}

/// TC-SUB-INV-4: a provider that was never registered cannot be removed.
#[test]
fn removing_an_unknown_provider_is_a_fault() {
    assert_eq!(
        only_fault(&[removed("ghost")]),
        "subagent/provider-removed names unknown provider \"ghost\""
    );
}

/// TC-SUB-INV-5: a run names its provider, itself and its child.
#[test]
fn a_run_missing_part_of_its_identity_is_a_fault() {
    for blank in ["provider", "run_id", "child_id"] {
        let mut identity = run("r-1");
        match blank {
            "provider" => identity.provider.clear(),
            "run_id" => identity.run_id.clear(),
            _ => identity.child_id.clear(),
        }
        let faults = subagent_lifecycle_faults(&[LifecycleEvent::RunStarted(identity)]);
        assert!(
            faults.contains(
                &"subagent/start provider, runId, and child id must be non-empty".to_owned()
            ),
            "blanking {blank} should be a fault, got {faults:?}"
        );
    }
}

/// TC-SUB-INV-6: an open run id is not reused.
#[test]
fn reusing_an_open_run_id_is_a_fault() {
    assert_eq!(
        only_fault(&[started("r-1"), started("r-1")]),
        "subagent/start repeated run id \"r-1\""
    );
}

/// TC-SUB-INV-7: a run cannot end without starting.
#[test]
fn ending_a_run_that_never_started_is_a_fault() {
    assert_eq!(
        only_fault(&[ended("r-9")]),
        "subagent/end has no matching subagent/start for run \"r-9\""
    );
}

/// TC-SUB-INV-8: an ending must describe the same run as its beginning.
///
/// The subtle rule. `subagent/end` repeats the identity rather than only the
/// id, and if the repeat disagrees then one of the two events is about a
/// different run — so a parent may be crediting an answer to the wrong child.
#[test]
fn an_ending_that_describes_a_different_run_is_a_fault() {
    let mut end = run("r-1");
    end.child_id = "somebody-else".into();
    let events = [started("r-1"), LifecycleEvent::RunEnded(end)];
    assert_eq!(
        only_fault(&events),
        "subagent/end identity diverges from subagent/start for run \"r-1\""
    );
}

/// TC-SUB-INV-9: a run id is reusable once its run has closed.
///
/// This port's own. The registry tracks *open* runs, and a backend that
/// numbers runs per child would legitimately reuse an id after the previous
/// one settled. Treating that as a repeat would report a fault on a correct
/// backend.
#[test]
fn a_run_id_may_be_reused_after_its_run_closed() {
    let events = [started("r-1"), ended("r-1"), started("r-1"), ended("r-1")];
    assert_eq!(subagent_lifecycle_faults(&events), Vec::<String>::new());
}

/// TC-SUB-INV-10: a run outliving its provider's removal is not a fault.
///
/// This port's own, and it pins a deliberate *absence*. Provider availability
/// is checked when a run is admitted, not for its whole life: a one-shot run
/// may outlive the removal of the provider that started it, and a resumed run
/// records the provider it began under without going through it again. A
/// stricter check here would report faults on both.
#[test]
fn a_run_may_outlive_the_provider_that_started_it() {
    let events = [
        added("spawn"),
        started("r-1"),
        removed("spawn"),
        ended("r-1"),
    ];
    assert_eq!(subagent_lifecycle_faults(&events), Vec::<String>::new());
}

/// TC-SUB-INV-11: a missing beginning is reported once, not twice.
///
/// This port's own, and it is the difference the fold makes. Upstream checks
/// the identity match even when no start was found — unreachable there,
/// because the missing-start failure throws first. A fold that reports
/// everything must not also report a divergence from a beginning that does not
/// exist, or every orphaned end would come with a second, meaningless fault.
#[test]
fn an_orphaned_ending_reports_one_fault_and_not_two() {
    let faults = subagent_lifecycle_faults(&[ended("r-9")]);
    assert_eq!(
        faults,
        ["subagent/end has no matching subagent/start for run \"r-9\""]
    );
}
