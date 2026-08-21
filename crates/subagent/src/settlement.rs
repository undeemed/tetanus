//! Turning a finished child run into an outcome, and releasing what it held.
//!
//! A one-shot delegated run is a background task from the parent's side: it is
//! started, it finishes, and something has to say what happened and free the
//! child's resources. This module is that step.
//!
//! Two rules make it worth its own module.
//!
//! **Disposal always happens.** Whatever the child's result was — an answer, a
//! refusal, or a transport that vanished — the run is released before the
//! outcome is reported. A settlement that returned early on a bad result would
//! leak a child process for every run that failed, which is precisely the
//! population most likely to be numerous.
//!
//! **A failure to release is itself a failure, and does not hide the first
//! one.** If the result failed *and* disposal failed, both survive in the
//! reported detail. Reporting only the disposal error would replace the reason
//! the run failed with a consequence of it.
//!
//! Only one-shot runs settle this way. A continuable child has no task and no
//! per-message outcome; it is ended, not settled.
//!
//! Parity: upstream `packages/subagent/subagent/src/run-settlement.ts`, pinned
//! by its `run-settlement.spec.ts`.

use std::fmt;

/// Why a child stopped.
///
/// Open rather than closed: a backend may report a reason this build has not
/// heard of, and the fold below treats an unknown reason as a failure carrying
/// its own name rather than refusing to settle the run at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// It finished and produced its answer.
    Completed,
    /// It was cancelled.
    Aborted,
    /// It failed.
    Error,
    /// It hit the output cap.
    MaxTokens,
    /// It declined.
    Refusal,
    /// Something this build does not know about.
    Other(String),
}

impl StopReason {
    /// The reason as it is written down.
    pub fn as_str(&self) -> &str {
        match self {
            StopReason::Completed => "completed",
            StopReason::Aborted => "aborted",
            StopReason::Error => "error",
            StopReason::MaxTokens => "max-tokens",
            StopReason::Refusal => "refusal",
            StopReason::Other(reason) => reason,
        }
    }

    /// Read a reason a backend reported.
    pub fn parse(reason: &str) -> Self {
        match reason {
            "completed" => StopReason::Completed,
            "aborted" => StopReason::Aborted,
            "error" => StopReason::Error,
            "max-tokens" => StopReason::MaxTokens,
            "refusal" => StopReason::Refusal,
            other => StopReason::Other(other.to_owned()),
        }
    }
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a child run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentResult {
    /// Its final answer.
    pub output: String,
    /// Why it stopped.
    pub stop_reason: StopReason,
}

/// What the parent records about a finished run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// It finished, and this is what it said.
    Completed {
        /// The child's final answer.
        output: String,
    },
    /// It was cancelled. There is no partial answer, because a cancelled run
    /// did not reach one.
    Killed,
    /// It failed, and this is why.
    Failed {
        /// What went wrong, in the child's own words where there are any.
        detail: String,
    },
}

/// Map a child's result onto the parent's outcome.
///
/// A completed run carries its answer. An abort is a kill, deliberately
/// without output: whatever the child had written when it was cancelled is not
/// an answer to the question. Every other reason is a failure named by that
/// reason, including one this build does not recognise — a run that stopped
/// for an unknown reason has still stopped, and refusing to classify it would
/// leave the parent waiting.
pub fn outcome_of(result: SubagentResult) -> RunOutcome {
    match result.stop_reason {
        StopReason::Completed => RunOutcome::Completed {
            output: result.output,
        },
        StopReason::Aborted => RunOutcome::Killed,
        other => RunOutcome::Failed {
            detail: other.as_str().to_owned(),
        },
    }
}

/// Settle one finished run: classify its result, then release it.
///
/// `result` is what the child reported, or the reason it could not be
/// collected. `dispose` releases the run and may itself fail.
///
/// Both failures survive when both happen, in that order, because the result
/// failure is the cause and the disposal failure is a second fact about the
/// same run — reporting only the second would replace the diagnosis with an
/// after-effect.
pub fn settle(result: Result<SubagentResult, String>, dispose: Result<(), String>) -> RunOutcome {
    let outcome = match result {
        Ok(result) => outcome_of(result),
        Err(fault) => RunOutcome::Failed { detail: fault },
    };

    let Err(dispose_fault) = dispose else {
        return outcome;
    };

    let prefix = match &outcome {
        RunOutcome::Failed { detail } => format!("{detail}; "),
        _ => String::new(),
    };
    RunOutcome::Failed {
        detail: format!("{prefix}dispose failed: {dispose_fault}"),
    }
}
