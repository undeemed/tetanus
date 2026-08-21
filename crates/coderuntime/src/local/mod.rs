//! The local backend: one worker thread per run, and a budget it cannot
//! outlive.
//!
//! **A run happens off the turn's thread.** The turn's executor is where every
//! other session is being served from; a program that computes for a second
//! must not be a second nobody else gets. So each run is a `std::thread` and
//! the async side waits on a channel.
//!
//! **A thread cannot be killed, so the evaluator agrees to stop.** This is the
//! difference from upstream worth reading twice. Node calls
//! `worker.terminate()` and the loop dies mid-iteration; Rust has no such call
//! for an OS thread, not in the standard library and not anywhere that is
//! sound. What this does instead is set a flag the evaluator reads on every
//! step, so `while (true) {}` ends at the next step and the thread returns on
//! its own. The cost is that a backend can only ever run a language whose
//! evaluator cooperates. The benefit is that the runaway case actually ends,
//! with the thread reclaimed rather than leaked for the life of the process.
//!
//! **Every run is a fresh world.** No state survives from one run to the next,
//! because there is no state to survive: the scope chain is built per run from
//! the bindings the request carried.
//!
//! **What this backend is not.** It executes no native code, opens no file,
//! makes no connection and starts no process, so there is nothing here for a
//! path fence or a sandbox mode to contain. A future backend that hands the
//! program to a real interpreter would need exactly that, and the parity note
//! names it as the follow-up rather than guessing at its shape.

pub mod eval;
pub mod program;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::types::{
    check_bindings, Abort, CodeRuntime, FailureKind, RunRequest, RunResult, SeamError,
};

pub use eval::Budget;

/// The language this backend evaluates, as [`CodeRuntime::language`] reports
/// it. Not `javascript`, and deliberately not named as though it were: see the
/// crate note.
pub const LANGUAGE: &str = "tetanus-script";

/// The substrate, as [`CodeRuntime::isolation`] reports it.
pub const ISOLATION: &str = "worker-thread";

/// How long a worker gets to notice the stop flag before the runtime gives up
/// waiting for it.
///
/// It is a diagnostic bound, not a containment one: the evaluator checks the
/// flag every step, so the only way to reach it is a host binding that has
/// blocked for ever - and the message says so rather than reporting a timeout
/// the program caused.
const REAP_GRACE: Duration = Duration::from_secs(5);

/// Runs programs on worker threads in this process.
pub struct LocalRuntime {
    budget: Budget,
    /// Workers that have not returned yet. A test reads it to prove a runaway
    /// run was reclaimed rather than abandoned.
    live: Arc<AtomicUsize>,
    /// Fires when the runtime is shut down, so every in-flight run stops.
    closing: Abort,
}

impl Default for LocalRuntime {
    fn default() -> Self {
        Self::new(Budget::default())
    }
}

impl LocalRuntime {
    pub fn new(budget: Budget) -> Self {
        Self {
            budget,
            live: Arc::new(AtomicUsize::new(0)),
            closing: Abort::new(),
        }
    }

    pub fn budget(&self) -> Budget {
        self.budget
    }

    /// How many worker threads are still running. Zero once every run this
    /// runtime started has finished, whichever way it finished.
    pub fn live_workers(&self) -> usize {
        self.live.load(Ordering::Acquire)
    }
}

/// Held by a worker for as long as it runs, so the count falls even if the
/// evaluation panics.
struct Census(Arc<AtomicUsize>);

impl Census {
    fn open(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(count))
    }
}

impl Drop for Census {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[async_trait::async_trait]
impl CodeRuntime for LocalRuntime {
    fn language(&self) -> &str {
        LANGUAGE
    }

    fn isolation(&self) -> &str {
        ISOLATION
    }

    async fn run(&self, request: RunRequest) -> Result<RunResult, SeamError> {
        // Seam misuse first: a namespace no backend can expose is the
        // caller's mistake, and it costs nothing to say so before a thread
        // exists.
        check_bindings(&request.bindings)?;

        if self.closing.is_stopped() {
            return Err(SeamError::Unsupported {
                runtime: LANGUAGE.to_string(),
                why: "this runtime has been shut down".to_string(),
            });
        }
        // A request that arrived already aborted never starts a worker.
        if request.abort.is_stopped() {
            return Ok(RunResult::failed(
                FailureKind::Abort,
                "the run was aborted before it started",
            ));
        }

        let budget = self.budget;
        let live = Arc::clone(&self.live);
        let closing = self.closing.clone();
        let abort = request.abort.clone();
        let (answer, wait) = tokio::sync::oneshot::channel();

        let started = Instant::now();
        let worker = std::thread::Builder::new()
            .name("tetanus-code-worker".to_string())
            .spawn(move || {
                let _census = Census::open(&live);
                let outcome = evaluate(&request, budget, &closing);
                // The receiver is gone when the caller stopped waiting; the
                // work is done either way and there is nobody to tell.
                let _ = answer.send(outcome);
            })
            .map_err(|source| SeamError::Unsupported {
                runtime: LANGUAGE.to_string(),
                why: format!("a worker thread could not be started: {source}"),
            })?;

        // The wall-clock ceiling is enforced from out here as well as inside
        // the evaluator, because a host binding that never returns is not
        // something the evaluator can interrupt: it is not running.
        let ceiling = budget.wall + REAP_GRACE;
        let settled = match tokio::time::timeout(ceiling, wait).await {
            Ok(Ok(mut result)) => {
                result.duration = started.elapsed();
                result
            }
            // The worker dropped its side without sending: it panicked, which
            // is this substrate's version of a worker that died.
            Ok(Err(_)) => RunResult {
                duration: started.elapsed(),
                ..RunResult::failed(
                    FailureKind::WorkerExit,
                    "the worker running this program stopped without answering",
                )
            },
            Err(_) => {
                abort.stop();
                RunResult {
                    duration: started.elapsed(),
                    ..RunResult::failed(
                        FailureKind::Timeout,
                        format!(
                            "the run did not end within {}ms of its ceiling; a host binding is \
                             not returning",
                            REAP_GRACE.as_millis()
                        ),
                    )
                }
            }
        };

        // Reclaim the thread rather than detaching it. It has already
        // finished in every path but the one above, where the stop flag has
        // just been set and the join is what proves it was noticed.
        let reaped = tokio::task::spawn_blocking(move || worker.join()).await;
        if matches!(reaped, Ok(Err(_))) {
            tracing::error!("a code worker panicked; the run is reported as worker-exit");
        }
        Ok(settled)
    }

    async fn shutdown(&self) {
        self.closing.stop();
    }
}

/// One evaluation, on the worker thread.
///
/// A panic in here is contained and reported as `worker-exit`: the evaluator
/// is this crate's code, so a panic is a bug in it, and the caller learns that
/// the substrate died rather than losing the turn to an unwind.
fn evaluate(request: &RunRequest, budget: Budget, closing: &Abort) -> RunResult {
    // One flag for the two things that stop a run: the caller's abort, and
    // the runtime shutting down under it.
    let stop = request.abort.and(closing);
    let contained = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let parsed = match program::parse(&request.program) {
            Ok(parsed) => parsed,
            // A program that will not parse is an exception, exactly as a
            // program that threw is: both are the model's to correct.
            Err(why) => {
                return RunResult::failed(
                    FailureKind::Exception,
                    format!("the program does not parse: {why}"),
                )
            }
        };
        let mut state = eval::Run::new(budget, stop.clone(), &request.bindings);
        match eval::run(&parsed, &mut state) {
            Ok(value) => {
                let logs = std::mem::take(&mut state.logs);
                match value.map(|value| value.to_json()).transpose() {
                    Ok(json) => {
                        let mut result = RunResult::ok(json, logs, state.elapsed());
                        if let Some(over) = over_cap(&result, budget) {
                            result = over;
                        }
                        result
                    }
                    Err(why) => RunResult {
                        logs,
                        ..RunResult::failed(
                            FailureKind::InvalidOutput,
                            format!("the program's value cannot be returned: {why}"),
                        )
                    },
                }
            }
            Err(stopped) => {
                let kind = stopped.kind();
                RunResult {
                    logs: std::mem::take(&mut state.logs),
                    ..RunResult::failed(kind, stopped.message())
                }
            }
        }
    }));

    contained.unwrap_or_else(|payload| {
        RunResult::failed(
            FailureKind::WorkerExit,
            format!(
                "the worker running this program panicked: {}",
                panic_message(payload)
            ),
        )
    })
}

/// Whether the finished result is over the output cap, and the failure that
/// says so.
///
/// The logs were metered as they were written; this is the second half - the
/// completion value, which is only known at the end - so the two together are
/// one ledger rather than two caps that each pass.
fn over_cap(result: &RunResult, budget: Budget) -> Option<RunResult> {
    let logged: usize = result.logs.iter().map(String::len).sum();
    let valued = result
        .value
        .as_ref()
        .map_or(0, |value| value.to_string().len());
    (logged + valued > budget.max_output_bytes).then(|| RunResult {
        logs: result.logs.clone(),
        ..RunResult::failed(
            FailureKind::OutputLimit,
            format!(
                "the program's output and value are {} bytes together, past the {} byte cap",
                logged + valued,
                budget.max_output_bytes
            ),
        )
    })
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "<unprintable panic payload>".to_string()
}
