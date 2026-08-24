//! Multi-step work that outlives the turn that asked for it.
//!
//! A turn is one exchange with a model. Some work is not that shape: a
//! migration, a sweep across a repository, a review pass - a sequence of named
//! steps that takes longer than anyone wants to hold a conversation open for,
//! and that has to survive the harness being restarted in the middle.
//!
//! **The journal is the progress record, not a status field.** Every boundary
//! a run crosses is appended, so what a surface renders, what a restart reads
//! and what a test asserts are the same events. A workflow with its progress
//! in memory would be one whose progress a crash erases, and the whole point
//! of a workflow is that it is the work a crash must not erase.
//!
//! **Cancellation lands at the next step boundary.** The same rule the turn
//! engine follows for an interrupt, and for the same reason: a step already
//! running is doing something, and abandoning it half-done would leave the
//! journal claiming work that neither finished nor failed. The run stops
//! *between* steps, where the record is honest.
//!
//! **Resuming is re-reading, never remembering.** [`resume_point`] folds the
//! journal to find the first step a run has not completed, so a restarted
//! harness continues from the record rather than from anything it kept. A
//! completed step is never run twice, which is what makes a workflow safe to
//! restart at all - the steps are not assumed idempotent, because most useful
//! ones are not.
//!
//! Parity: upstream `packages/workflow/workflow` and `workflow-worker-thread`,
//! pinned by their `workflow.spec.ts` and `workflow-worker-thread.spec.ts`.
//! Upstream's workflow is a JavaScript script run in a worker thread, whose
//! steps are `agent()` calls it makes as it executes; tetanus has no script
//! runtime, so what ports is the part that is not JavaScript's - the declared
//! sequence, the durable progress, the cancellation point and the resume - and
//! the script body, its realm isolation and its `agent()` concurrency have
//! nothing to restate. Its `phase()` grouping is here as the step's own name,
//! because a tetanus step is declared rather than discovered.

use std::sync::Arc;

use serde_json::Value;

use tetanus_session::{SessionEvent, SessionLog};

use crate::interrupt::Interrupt;

/// The durable types a run writes.
///
/// Contract section 4.3.2: the vocabulary grows, and a surface passes an
/// unknown type through. None of these derives to a model message - a
/// workflow's progress is a fact about the harness, not something the model
/// said.
pub mod topic {
    pub const WORKFLOW_START: &str = "workflow/start";
    pub const WORKFLOW_STEP_START: &str = "workflow/step-start";
    pub const WORKFLOW_STEP_END: &str = "workflow/step-end";
    pub const WORKFLOW_END: &str = "workflow/end";
}

/// One declared step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowStep {
    /// Stable within its workflow: it is what a resume matches on, so renaming
    /// a step makes a half-finished run start it again rather than skip it.
    pub name: String,
    /// One line about what this step does.
    pub detail: String,
}

impl WorkflowStep {
    pub fn new(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            detail: detail.into(),
        }
    }
}

/// A declared sequence of steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    pub steps: Vec<WorkflowStep>,
}

/// How one step ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// It did its work, and this is what it has to say about it.
    Done(String),
    /// It broke. A failed step ends the run: the steps are a sequence, so a
    /// later one is entitled to assume the earlier ones happened.
    Failed(String),
}

/// Whoever actually performs a step.
///
/// A trait rather than a closure because the two useful implementations are
/// very different - one drives a model, one is a deterministic script a test
/// can assert against - and because a step needs to be able to fail without
/// the failure being the runner's problem to encode.
#[async_trait::async_trait]
pub trait StepRunner: Send + Sync {
    /// Perform one step. `index` is its position in the declared sequence.
    async fn run(&self, run_id: &str, index: usize, step: &WorkflowStep) -> StepOutcome;
}

/// Why a run settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowStopReason {
    /// Every step completed.
    Completed,
    /// Stopped at a step boundary because someone asked.
    Cancelled,
    /// A step failed.
    Failed,
}

/// What a run has to say for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowOutcome {
    pub run_id: String,
    /// How many steps this call completed. A resumed run counts its own, not
    /// the ones an earlier process already did.
    pub steps_completed: usize,
    /// How far the run has got in total, across every process that has worked
    /// on it.
    pub steps_done: usize,
    pub stop_reason: WorkflowStopReason,
    /// The failing step's words, when one failed.
    pub error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error(transparent)]
    Log(#[from] tetanus_session::SessionError),
    #[error("a workflow needs at least one step")]
    NoSteps,
    /// Only a *completed* run refuses a resume. A cancelled or failed one has
    /// an end on the journal too, and resuming it is the whole point.
    #[error("workflow run {0:?} already completed; there is nothing left to resume")]
    AlreadyCompleted(String),
    #[error(
        "the journal holds run {run_id:?} at step {done} of a workflow declaring {declared}: it \
         is not the workflow this run started"
    )]
    NotTheSameWorkflow {
        run_id: String,
        done: usize,
        declared: usize,
    },
}

/// Run a workflow from the beginning, recording every boundary.
pub async fn run(
    log: &dyn SessionLog,
    interrupt: &Arc<Interrupt>,
    workflow: &Workflow,
    runner: &dyn StepRunner,
    run_id: &str,
) -> Result<WorkflowOutcome, WorkflowError> {
    if workflow.steps.is_empty() {
        return Err(WorkflowError::NoSteps);
    }
    log.append(
        topic::WORKFLOW_START,
        serde_json::json!({
            "run_id": run_id,
            "workflow": workflow.name,
            "description": workflow.description,
            "steps": workflow.steps.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        }),
    )?;
    drive(log, interrupt, workflow, runner, run_id, 0).await
}

/// Continue a run the journal already holds, from the first step it has not
/// completed.
///
/// A run that *completed* is refused rather than restarted: it already has an
/// outcome, and a second one would make "how did this end" a question with two
/// answers - the rule the job store keeps for the same reason.
///
/// A cancelled or failed run is not that. It has an end on the journal too,
/// and continuing it is exactly what a resume is for: the end records why the
/// run stopped, not that it is finished with.
pub async fn resume(
    log: &dyn SessionLog,
    interrupt: &Arc<Interrupt>,
    workflow: &Workflow,
    runner: &dyn StepRunner,
    run_id: &str,
) -> Result<WorkflowOutcome, WorkflowError> {
    if workflow.steps.is_empty() {
        return Err(WorkflowError::NoSteps);
    }
    let events = log.events();
    if finished(&events, run_id) == Some(WorkflowStopReason::Completed) {
        return Err(WorkflowError::AlreadyCompleted(run_id.to_string()));
    }
    let from = resume_point(&events, run_id);
    if from > workflow.steps.len() {
        return Err(WorkflowError::NotTheSameWorkflow {
            run_id: run_id.to_string(),
            done: from,
            declared: workflow.steps.len(),
        });
    }
    drive(log, interrupt, workflow, runner, run_id, from).await
}

/// The step loop, shared by a fresh run and a resumed one so both write the
/// same boundaries in the same order.
async fn drive(
    log: &dyn SessionLog,
    interrupt: &Arc<Interrupt>,
    workflow: &Workflow,
    runner: &dyn StepRunner,
    run_id: &str,
    from: usize,
) -> Result<WorkflowOutcome, WorkflowError> {
    let mut completed = 0;
    let mut done = from;
    let mut reason = WorkflowStopReason::Completed;
    let mut error = None;

    for (index, step) in workflow.steps.iter().enumerate().skip(from) {
        // Checked between steps, never inside one: a step already running is
        // doing something, and abandoning it half-done would leave the journal
        // claiming work that neither finished nor failed.
        if interrupt.stopped() {
            reason = WorkflowStopReason::Cancelled;
            break;
        }
        log.append(
            topic::WORKFLOW_STEP_START,
            serde_json::json!({
                "run_id": run_id,
                "step": index,
                "name": step.name,
                "detail": step.detail,
            }),
        )?;
        let outcome = runner.run(run_id, index, step).await;
        let (ok, said) = match &outcome {
            StepOutcome::Done(said) => (true, said.clone()),
            StepOutcome::Failed(said) => (false, said.clone()),
        };
        log.append(
            topic::WORKFLOW_STEP_END,
            serde_json::json!({
                "run_id": run_id,
                "step": index,
                "name": step.name,
                "ok": ok,
                "output": said,
            }),
        )?;
        if !ok {
            reason = WorkflowStopReason::Failed;
            error = Some(said);
            break;
        }
        completed += 1;
        done = index + 1;
    }

    log.append(
        topic::WORKFLOW_END,
        serde_json::json!({
            "run_id": run_id,
            "workflow": workflow.name,
            "stop_reason": reason,
            "steps_done": done,
            "error": error,
        }),
    )?;

    Ok(WorkflowOutcome {
        run_id: run_id.to_string(),
        steps_completed: completed,
        steps_done: done,
        stop_reason: reason,
        error,
    })
}

/// The index of the first step `run_id` has not completed.
///
/// A fold over the journal and nothing else, which is what makes a resume
/// re-reading rather than remembering. A step whose `workflow/step-end` says
/// it failed is *not* completed, so a resume runs it again - the failure is
/// the reason the run stopped, and retrying it is the only thing a resume
/// could usefully do.
pub fn resume_point(events: &[SessionEvent], run_id: &str) -> usize {
    let mut next = 0;
    for event in events {
        if event.ty != topic::WORKFLOW_STEP_END || field(event, "run_id") != Some(run_id) {
            continue;
        }
        let ok = event
            .data
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let index = event.data.get("step").and_then(Value::as_u64).unwrap_or(0) as usize;
        if ok {
            next = next.max(index + 1);
        }
    }
    next
}

/// How a run ended, if the journal says it has.
pub fn finished(events: &[SessionEvent], run_id: &str) -> Option<WorkflowStopReason> {
    events
        .iter()
        .rev()
        .find(|event| event.ty == topic::WORKFLOW_END && field(event, "run_id") == Some(run_id))
        .and_then(|event| serde_json::from_value(event.data.get("stop_reason")?.clone()).ok())
}

/// Every run the journal mentions, in the order they started.
pub fn runs(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|event| event.ty == topic::WORKFLOW_START)
        .filter_map(|event| field(event, "run_id").map(str::to_string))
        .collect()
}

/// A run the journal opened and never closed - what a restart has to look for.
pub fn unfinished(events: &[SessionEvent]) -> Vec<String> {
    runs(events)
        .into_iter()
        .filter(|run_id| finished(events, run_id).is_none())
        .collect()
}

fn field<'a>(event: &'a SessionEvent, key: &str) -> Option<&'a str> {
    event.data.get(key).and_then(Value::as_str)
}
