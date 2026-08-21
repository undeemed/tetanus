//! Conformance: turning a finished child run into an outcome.
//!
//! Feature under test: `tetanus_subagent::settlement` — the map from a child's
//! stop reason onto the parent's outcome, and what happens when releasing the
//! run fails too.
//!
//! Ported from upstream
//! `packages/subagent/subagent/tests/run-settlement.spec.ts`.
//! Case ids TC-SUB-SETTLE-1..9. The last four are this port's own.

use tetanus_subagent::settlement::{outcome_of, settle, RunOutcome, StopReason, SubagentResult};

fn result(stop_reason: StopReason) -> SubagentResult {
    SubagentResult {
        output: "partial".into(),
        stop_reason,
    }
}

fn failed(detail: &str) -> RunOutcome {
    RunOutcome::Failed {
        detail: detail.to_owned(),
    }
}

/// TC-SUB-SETTLE-1: every stop reason maps onto its outcome.
#[test]
fn every_stop_reason_maps_onto_its_outcome() {
    let cases = [
        (
            StopReason::Completed,
            RunOutcome::Completed {
                output: "partial".into(),
            },
        ),
        (StopReason::Aborted, RunOutcome::Killed),
        (StopReason::Error, failed("error")),
        (StopReason::MaxTokens, failed("max-tokens")),
        (StopReason::Refusal, failed("refusal")),
    ];
    for (reason, expected) in cases {
        assert_eq!(
            settle(Ok(result(reason.clone())), Ok(())),
            expected,
            "for {reason}"
        );
    }
}

/// TC-SUB-SETTLE-2: a reason this build does not know is still a failure, and
/// carries its own name.
///
/// A run that stopped for an unrecognised reason has still stopped. Refusing
/// to classify it would leave the parent waiting on a child that is gone.
#[test]
fn an_unknown_stop_reason_is_a_failure_named_by_itself() {
    assert_eq!(
        settle(Ok(result(StopReason::Other("paused".into()))), Ok(())),
        failed("paused")
    );
}

/// TC-SUB-SETTLE-3: a result that could not be collected is a failure.
#[test]
fn a_result_that_could_not_be_collected_is_a_failure() {
    assert_eq!(
        settle(Err("transport gone".into()), Ok(())),
        failed("transport gone")
    );
}

/// TC-SUB-SETTLE-4: failing to release the run is itself a failure, even when
/// the child succeeded.
#[test]
fn a_disposal_failure_fails_an_otherwise_good_run() {
    assert_eq!(
        settle(Ok(result(StopReason::Completed)), Err("reap failed".into())),
        failed("dispose failed: reap failed")
    );
}

/// TC-SUB-SETTLE-5: when both fail, both survive, cause first.
///
/// Reporting only the disposal error would replace the reason the run failed
/// with an after-effect of it.
#[test]
fn both_failures_survive_with_the_cause_first() {
    assert_eq!(
        settle(Err("result failed".into()), Err("reap failed".into())),
        failed("result failed; dispose failed: reap failed")
    );
}

/// TC-SUB-SETTLE-6: an abort reports no output.
///
/// This port's own. The result carries text — a cancelled child had usually
/// written something — and reporting it as the answer would present a partial
/// draft as a completed one. `Killed` has no output field at all, so this is a
/// rule the type enforces rather than a value the caller must remember to drop.
#[test]
fn an_aborted_run_reports_no_output_even_though_it_wrote_some() {
    let cancelled = SubagentResult {
        output: "half an answer".into(),
        stop_reason: StopReason::Aborted,
    };
    assert_eq!(outcome_of(cancelled), RunOutcome::Killed);
}

/// TC-SUB-SETTLE-7: a failure does not smuggle the child's text out as if it
/// were an answer.
///
/// This port's own, and the same rule as TC-SUB-SETTLE-6 for the other
/// non-completing reasons: the detail names the reason, never the output.
#[test]
fn a_failed_run_reports_its_reason_and_not_its_text() {
    for reason in [
        StopReason::Error,
        StopReason::MaxTokens,
        StopReason::Refusal,
    ] {
        let outcome = outcome_of(SubagentResult {
            output: "a draft nobody asked for".into(),
            stop_reason: reason.clone(),
        });
        assert_eq!(outcome, failed(reason.as_str()), "for {reason}");
    }
}

/// TC-SUB-SETTLE-8: a completed run with nothing to say still completes.
///
/// This port's own. Empty output and a failure are different facts, and
/// collapsing them would turn a child that legitimately had no answer into a
/// run the parent reports as broken.
#[test]
fn a_completed_run_with_no_output_still_completes() {
    let quiet = SubagentResult {
        output: String::new(),
        stop_reason: StopReason::Completed,
    };
    assert_eq!(
        outcome_of(quiet),
        RunOutcome::Completed {
            output: String::new()
        }
    );
}

/// TC-SUB-SETTLE-9: a stop reason survives a round trip through its written
/// form.
///
/// This port's own. The reason crosses a process boundary as text from an
/// out-of-process backend, and a reason that did not round-trip would be
/// reclassified as unknown on the way back — turning a clean completion into
/// a failure named `completed`.
#[test]
fn a_stop_reason_survives_a_round_trip_through_text() {
    for reason in [
        StopReason::Completed,
        StopReason::Aborted,
        StopReason::Error,
        StopReason::MaxTokens,
        StopReason::Refusal,
        StopReason::Other("paused".into()),
    ] {
        assert_eq!(StopReason::parse(reason.as_str()), reason, "for {reason}");
    }
}
