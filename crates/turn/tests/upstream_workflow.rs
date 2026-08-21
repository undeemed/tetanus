//! Test Design Specification: multi-step work outside a turn, ported.
//!
//! Features under test: `tetanus_turn::workflow` - the boundaries a run
//! records, where a cancellation lands, and what a restart continues from.
//! Upstream pins the same decisions in
//! `packages/workflow/workflow/tests/workflow.spec.ts` and
//! `workflow-worker-thread/tests/workflow-worker-thread.spec.ts`.
//!
//! Approach: a journal on disk and a scripted runner, so a case states what
//! each step does rather than arranging for a model to do it. A restart is
//! modelled the honest way - the journal is replayed from disk into a second
//! run - so a resume can only be reading the record.
//!
//! What is not restated, and why. Upstream's workflow is a JavaScript script
//! run in a worker thread, whose steps are the `agent()` calls it makes as it
//! executes. tetanus has no script runtime, so its script parsing, realm
//! isolation, `agent()` concurrency, and the worker's death and grace paths
//! have nothing to restate. What ports is the part that is not JavaScript's:
//! the declared sequence, the durable progress, the cancellation point and the
//! resume. Its `phase()` grouping is the step's own name here, because a
//! tetanus step is declared rather than discovered.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::{Arc, Mutex};

use tetanus_core::EventBus;
use tetanus_session::{replay, JsonlSessionLog, SessionLog};
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::workflow::{
    self, resume_point, run, StepOutcome, StepRunner, Workflow, WorkflowError, WorkflowStep,
    WorkflowStopReason,
};

fn journal(name: &str) -> (Arc<JsonlSessionLog>, std::path::PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join(format!("{name}.jsonl"));
    let log = JsonlSessionLog::create(name, &path, EventBus::new()).expect("journal");
    (log, path, dir)
}

fn three_steps() -> Workflow {
    Workflow {
        name: "tidy".into(),
        description: "three things in order".into(),
        steps: vec![
            WorkflowStep::new("read", "read the tree"),
            WorkflowStep::new("edit", "change what needs changing"),
            WorkflowStep::new("verify", "run the tests"),
        ],
    }
}

/// A runner that records which steps it was asked to do, and can be told to
/// fail one of them or to cancel while one runs.
struct Scripted {
    seen: Arc<Mutex<Vec<String>>>,
    fail_at: Option<usize>,
    cancel_at: Option<(usize, Arc<Interrupt>)>,
}

impl Scripted {
    fn new() -> Self {
        Self {
            seen: Arc::new(Mutex::new(Vec::new())),
            fail_at: None,
            cancel_at: None,
        }
    }
    fn failing_at(mut self, index: usize) -> Self {
        self.fail_at = Some(index);
        self
    }
    fn cancelling_during(mut self, index: usize, interrupt: Arc<Interrupt>) -> Self {
        self.cancel_at = Some((index, interrupt));
        self
    }
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl StepRunner for Scripted {
    async fn run(&self, _run_id: &str, index: usize, step: &WorkflowStep) -> StepOutcome {
        self.seen.lock().unwrap().push(step.name.clone());
        if let Some((at, interrupt)) = &self.cancel_at {
            if *at == index {
                interrupt.stop();
            }
        }
        if self.fail_at == Some(index) {
            return StepOutcome::Failed(format!("{} broke", step.name));
        }
        StepOutcome::Done(format!("{} done", step.name))
    }
}

fn types(log: &dyn SessionLog) -> Vec<String> {
    log.events().into_iter().map(|event| event.ty).collect()
}

/// TC-PORT-FLOW-1: a run records every boundary it crosses, in order.
///
/// Upstream: `workflow.spec.ts`, "emits start, per-agent boundaries and end".
///
/// Expected: one start, a start/end pair per step, one end; the outcome says
/// completed; every step ran once, in declaration order.
#[tokio::test]
async fn a_run_records_every_boundary_in_order() {
    let (log, _path, _dir) = journal("boundaries");
    let interrupt = Interrupt::new();
    let runner = Scripted::new();

    let outcome = run(log.as_ref(), &interrupt, &three_steps(), &runner, "r-1")
        .await
        .expect("run");

    assert_eq!(outcome.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(outcome.steps_completed, 3);
    assert_eq!(outcome.steps_done, 3);
    assert!(outcome.error.is_none());
    assert_eq!(runner.seen(), vec!["read", "edit", "verify"]);
    assert_eq!(
        types(log.as_ref()),
        vec![
            "workflow/start",
            "workflow/step-start",
            "workflow/step-end",
            "workflow/step-start",
            "workflow/step-end",
            "workflow/step-start",
            "workflow/step-end",
            "workflow/end",
        ]
    );
}

/// TC-PORT-FLOW-2: a failing step ends the run, and the journal says which.
///
/// The steps are a sequence, so a later one is entitled to assume the earlier
/// ones happened; carrying on past a failure would break that assumption
/// silently.
///
/// Expected: the third step never runs; the outcome carries the failing step's
/// words; the end records `failed` and how far it got.
#[tokio::test]
async fn a_failing_step_ends_the_run() {
    let (log, _path, _dir) = journal("failing");
    let interrupt = Interrupt::new();
    let runner = Scripted::new().failing_at(1);

    let outcome = run(log.as_ref(), &interrupt, &three_steps(), &runner, "r-1")
        .await
        .expect("run");

    assert_eq!(outcome.stop_reason, WorkflowStopReason::Failed);
    assert_eq!(outcome.error.as_deref(), Some("edit broke"));
    assert_eq!(outcome.steps_done, 1);
    assert_eq!(runner.seen(), vec!["read", "edit"], "verify never ran");

    let events = log.events();
    let end = events.last().expect("an end");
    assert_eq!(end.ty, "workflow/end");
    assert_eq!(end.data["stop_reason"], serde_json::json!("failed"));
    assert_eq!(end.data["steps_done"], serde_json::json!(1));
}

/// TC-PORT-FLOW-3: a cancelled run stops at its next checkpoint, and its
/// journal says so.
///
/// The acceptance claim. Cancellation lands *between* steps, the same rule the
/// turn engine follows: a step already running is doing something, and
/// abandoning it half-done would leave the journal claiming work that neither
/// finished nor failed.
///
/// Expected: the step that was running when the cancel arrived finishes and is
/// recorded done; the next one never starts; the end records `cancelled`.
#[tokio::test]
async fn a_cancelled_run_stops_at_its_next_checkpoint_and_says_so() {
    let (log, path, _dir) = journal("cancelled");
    let interrupt = Interrupt::new();
    let runner = Scripted::new().cancelling_during(0, Arc::clone(&interrupt));

    let outcome = run(log.as_ref(), &interrupt, &three_steps(), &runner, "r-1")
        .await
        .expect("run");

    assert_eq!(outcome.stop_reason, WorkflowStopReason::Cancelled);
    assert_eq!(
        runner.seen(),
        vec!["read"],
        "the running step finished; the next never started"
    );
    assert_eq!(outcome.steps_done, 1, "the step that ran is recorded done");

    let events = log.events();
    let end = events.last().expect("an end");
    assert_eq!(end.ty, "workflow/end");
    assert_eq!(end.data["stop_reason"], serde_json::json!("cancelled"));

    // The step that was in flight is recorded as done, not left dangling.
    let ended: Vec<_> = events
        .iter()
        .filter(|event| event.ty == "workflow/step-end")
        .collect();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].data["ok"], serde_json::json!(true));

    log.flush().unwrap();
    assert_eq!(
        workflow::finished(&replay(&path).unwrap(), "r-1"),
        Some(WorkflowStopReason::Cancelled),
        "the journal on disk says so too"
    );
}

/// TC-PORT-FLOW-4: a restart continues from the record, and never repeats a
/// completed step.
///
/// The steps are not assumed idempotent, because most useful ones are not, so
/// this is the property that makes a workflow safe to restart at all.
///
/// Expected: the resumed run starts at the step after the last completed one;
/// the whole run is complete; and the second runner never saw the first
/// runner's steps.
#[tokio::test]
async fn a_restart_continues_from_the_record() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("resumed.jsonl");

    // First process: cancelled after the first step.
    {
        let log = JsonlSessionLog::create("resumed", &path, EventBus::new()).expect("journal");
        let interrupt = Interrupt::new();
        let runner = Scripted::new().cancelling_during(0, Arc::clone(&interrupt));
        let outcome = run(log.as_ref(), &interrupt, &three_steps(), &runner, "r-1")
            .await
            .expect("run");
        assert_eq!(outcome.stop_reason, WorkflowStopReason::Cancelled);
        log.flush().unwrap();
    }

    // Second process: the same journal, nothing in memory.
    let events = replay(&path).expect("replay");
    assert_eq!(resume_point(&events, "r-1"), 1);

    let log = JsonlSessionLog::create("resumed", &path, EventBus::new()).expect("reopen");
    let interrupt = Interrupt::new();
    let runner = Scripted::new();
    let outcome = workflow::resume(log.as_ref(), &interrupt, &three_steps(), &runner, "r-1")
        .await
        .expect("resume");

    assert_eq!(outcome.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(outcome.steps_completed, 2, "this process did two of them");
    assert_eq!(outcome.steps_done, 3, "the run as a whole is complete");
    assert_eq!(
        runner.seen(),
        vec!["edit", "verify"],
        "the completed step was not run again"
    );
}

/// TC-PORT-FLOW-5: a resume runs a failed step again.
///
/// A step whose end says it failed is not completed, and retrying it is the
/// only thing a resume could usefully do - the failure is why the run stopped.
///
/// Expected: the resume point is the failed step's own index, and the resumed
/// run starts there.
#[tokio::test]
async fn a_resume_runs_a_failed_step_again() {
    let (log, _path, _dir) = journal("retried");
    let interrupt = Interrupt::new();

    let failing = Scripted::new().failing_at(1);
    run(log.as_ref(), &interrupt, &three_steps(), &failing, "r-1")
        .await
        .expect("run");
    assert_eq!(resume_point(&log.events(), "r-1"), 1);

    let recovered = Scripted::new();
    let outcome = workflow::resume(log.as_ref(), &interrupt, &three_steps(), &recovered, "r-1")
        .await
        .expect("resume");

    assert_eq!(outcome.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(recovered.seen(), vec!["edit", "verify"]);
}

/// TC-PORT-FLOW-6: a run that *completed* is not resumed.
///
/// It has an outcome, and a second one would make "how did this end" a
/// question with two answers - the rule the job store keeps for the same
/// reason. Only completion refuses: TC-PORT-FLOW-4 and -5 resume a cancelled
/// and a failed run, which is what a resume is for.
///
/// Expected: `AlreadyCompleted`, and nothing appended.
#[tokio::test]
async fn a_completed_run_is_not_resumed() {
    let (log, _path, _dir) = journal("done");
    let interrupt = Interrupt::new();
    run(
        log.as_ref(),
        &interrupt,
        &three_steps(),
        &Scripted::new(),
        "r-1",
    )
    .await
    .expect("run");
    let before = log.events().len();

    let refused = workflow::resume(
        log.as_ref(),
        &interrupt,
        &three_steps(),
        &Scripted::new(),
        "r-1",
    )
    .await
    .expect_err("already finished");

    assert!(matches!(refused, WorkflowError::AlreadyCompleted(id) if id == "r-1"));
    assert_eq!(log.events().len(), before, "nothing was appended");
}

/// TC-PORT-FLOW-7: which runs a restart has to look at.
///
/// A restarted harness needs to know what was in flight without holding
/// anything from before, so this is a fold over the journal like every other
/// answer here.
///
/// Expected: only the run with no end is reported unfinished, across two runs
/// on one journal.
#[tokio::test]
async fn a_restart_can_tell_which_runs_were_in_flight() {
    let (log, path, _dir) = journal("mixed");
    let interrupt = Interrupt::new();

    run(
        log.as_ref(),
        &interrupt,
        &three_steps(),
        &Scripted::new(),
        "finished",
    )
    .await
    .expect("run");

    // A run whose process died: a start and a step, and no end.
    log.append(
        "workflow/start",
        serde_json::json!({ "run_id": "in-flight", "workflow": "tidy", "steps": ["read"] }),
    )
    .unwrap();
    log.append(
        "workflow/step-end",
        serde_json::json!({ "run_id": "in-flight", "step": 0, "name": "read", "ok": true }),
    )
    .unwrap();
    log.flush().unwrap();

    let events = replay(&path).expect("replay");
    assert_eq!(workflow::runs(&events), vec!["finished", "in-flight"]);
    assert_eq!(workflow::unfinished(&events), vec!["in-flight"]);
    assert_eq!(resume_point(&events, "in-flight"), 1);
    assert_eq!(
        workflow::finished(&events, "finished"),
        Some(WorkflowStopReason::Completed)
    );
}

/// TC-PORT-FLOW-8: two runs on one journal do not read each other's progress.
///
/// Every fold here matches on `run_id`, so a second run of the same workflow
/// on the same session starts from the beginning rather than inheriting the
/// first one's steps.
///
/// Expected: the second run's resume point is zero while the first is at three.
#[tokio::test]
async fn two_runs_on_one_journal_are_independent() {
    let (log, _path, _dir) = journal("two");
    let interrupt = Interrupt::new();
    run(
        log.as_ref(),
        &interrupt,
        &three_steps(),
        &Scripted::new(),
        "first",
    )
    .await
    .expect("run");

    let events = log.events();
    assert_eq!(resume_point(&events, "first"), 3);
    assert_eq!(resume_point(&events, "second"), 0);

    let runner = Scripted::new();
    let outcome = run(log.as_ref(), &interrupt, &three_steps(), &runner, "second")
        .await
        .expect("run");
    assert_eq!(outcome.steps_completed, 3);
    assert_eq!(runner.seen(), vec!["read", "edit", "verify"]);
}

/// TC-PORT-FLOW-9: a workflow with no steps is refused where it is declared.
///
/// Expected: `NoSteps`, and nothing appended - a run that could never make
/// progress should not leave a start on the journal.
#[tokio::test]
async fn a_workflow_with_no_steps_is_refused() {
    let (log, _path, _dir) = journal("empty");
    let interrupt = Interrupt::new();
    let empty = Workflow {
        name: "nothing".into(),
        description: "no steps".into(),
        steps: Vec::new(),
    };

    let refused = run(log.as_ref(), &interrupt, &empty, &Scripted::new(), "r-1")
        .await
        .expect_err("no steps");

    assert!(matches!(refused, WorkflowError::NoSteps));
    assert!(log.events().is_empty(), "nothing was appended");
}

/// TC-PORT-FLOW-10: an interrupt that arrives before the first step stops the
/// run without starting anything.
///
/// Expected: no step ran, the outcome is cancelled, and the journal holds a
/// start and an end with nothing between them.
#[tokio::test]
async fn an_interrupt_before_the_first_step_starts_nothing() {
    let (log, _path, _dir) = journal("early");
    let interrupt = Interrupt::new();
    interrupt.stop();
    let runner = Scripted::new();

    let outcome = run(log.as_ref(), &interrupt, &three_steps(), &runner, "r-1")
        .await
        .expect("run");

    assert_eq!(outcome.stop_reason, WorkflowStopReason::Cancelled);
    assert_eq!(outcome.steps_done, 0);
    assert!(runner.seen().is_empty());
    assert_eq!(types(log.as_ref()), vec!["workflow/start", "workflow/end"]);
}
