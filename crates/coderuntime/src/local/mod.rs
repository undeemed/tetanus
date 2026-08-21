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
use std::time::Instant;

use std::collections::BTreeMap;
use std::sync::mpsc;

use crate::types::{
    check_bindings, Abort, AsyncBinding, CodeRuntime, FailureKind, HostCall, Member, Namespace,
    RunRequest, RunResult, SeamError,
};

pub use eval::Budget;

/// The language this backend evaluates, as [`CodeRuntime::language`] reports
/// it. Not `javascript`, and deliberately not named as though it were: see the
/// crate note.
pub const LANGUAGE: &str = "tetanus-script";

/// The substrate, as [`CodeRuntime::isolation`] reports it.
pub const ISOLATION: &str = "worker-thread";

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
        // The bridge: the worker sends host-answered calls down this, and the
        // wait below serves them while the program is blocked on the reply.
        let (calls, asked) = tokio::sync::mpsc::unbounded_channel::<HostCall>();
        let members = host_members(&request.bindings);

        let started = Instant::now();
        let worker = std::thread::Builder::new()
            .name("tetanus-code-worker".to_string())
            .spawn(move || {
                let _census = Census::open(&live);
                let outcome = evaluate(&request, budget, &closing, calls);
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
        let (settled, finished) = settle(Waiting {
            answer: wait,
            asked,
            members,
            budget,
            started,
            abort: &abort,
        })
        .await;
        reap(worker, finished);
        Ok(settled)
    }

    async fn shutdown(&self) {
        self.closing.stop();
    }
}

/// What one wait needs: the worker's answer, its host calls, and the bounds.
struct Waiting<'a> {
    answer: tokio::sync::oneshot::Receiver<RunResult>,
    asked: tokio::sync::mpsc::UnboundedReceiver<HostCall>,
    /// The host-answered members, by namespace and name.
    members: BTreeMap<(String, String), AsyncBinding>,
    budget: Budget,
    started: Instant,
    abort: &'a Abort,
}

/// Wait for the worker's answer, serving its host calls, or stop waiting.
///
/// Answers what to report and whether the worker is known to have finished -
/// a thread that has answered can be joined at once, and one that has not is
/// blocked on something, where joining would make this call wait exactly as
/// long as that does.
async fn settle(waiting: Waiting<'_>) -> (RunResult, bool) {
    let Waiting {
        mut answer,
        mut asked,
        members,
        budget,
        started,
        abort,
    } = waiting;
    let ceiling = tokio::time::sleep(budget.wall + budget.reap_grace);
    tokio::pin!(ceiling);

    loop {
        tokio::select! {
            // A host-answered member. The program is blocked on the reply, so
            // this is the only thing that will move it along.
            Some(call) = asked.recv() => serve(&members, call).await,
            settled = &mut answer => return match settled {
                Ok(mut result) => {
                    result.duration = started.elapsed();
                    (result, true)
                }
                // The worker dropped its side without sending: it panicked,
                // which is this substrate's version of a worker that died.
                Err(_) => (
                    RunResult {
                        duration: started.elapsed(),
                        ..RunResult::failed(
                            FailureKind::WorkerExit,
                            "the worker running this program stopped without answering",
                        )
                    },
                    true,
                ),
            },
            () = &mut ceiling => {
                abort.stop();
                return (
                    RunResult {
                        duration: started.elapsed(),
                        ..RunResult::failed(
                            FailureKind::Timeout,
                            format!(
                                "the run did not end within {}ms of its ceiling; a host binding \
                                 is not returning",
                                budget.reap_grace.as_millis()
                            ),
                        )
                    },
                    false,
                );
            }
        }
    }
}

/// Answer one call from the program.
///
/// A member the request never declared is answered as a failure rather than
/// left hanging: the worker is blocked on this reply, and silence would turn a
/// wiring mistake into a run that only the ceiling ends.
async fn serve(members: &BTreeMap<(String, String), AsyncBinding>, call: HostCall) {
    let key = (call.namespace.clone(), call.member.clone());
    let answer = match members.get(&key) {
        Some(body) => body(call.argument).await,
        None => Err(format!(
            "{}.{} is not a member this run was given",
            call.namespace, call.member
        )),
    };
    // The worker is gone when the run has already been given up on; the
    // answer has nowhere to go and nothing is owed.
    let _ = call.reply.send(answer);
}

/// The host-answered members of a request, flattened to what [`serve`] looks
/// up.
fn host_members(bindings: &[Namespace]) -> BTreeMap<(String, String), AsyncBinding> {
    bindings
        .iter()
        .flat_map(|namespace| {
            namespace
                .functions
                .iter()
                .filter_map(|(name, member)| match member {
                    Member::Host(body) => {
                        Some(((namespace.global.clone(), name.clone()), Arc::clone(body)))
                    }
                    Member::Here(_) => None,
                })
        })
        .collect()
}

/// Reclaim the worker thread.
///
/// A worker that has answered is joined here, where a caller can see that it
/// happened. One still inside a host binding is joined in the background: it
/// stays counted as live for as long as that lasts, because `live_workers`
/// should tell the truth rather than a comfortable number, and this is what
/// stops the thread being leaked when the binding finally returns.
fn reap(worker: std::thread::JoinHandle<()>, finished: bool) {
    if finished {
        if worker.join().is_err() {
            tracing::error!("a code worker panicked; the run is reported as worker-exit");
        }
        return;
    }
    std::thread::Builder::new()
        .name("tetanus-code-reaper".to_string())
        .spawn(move || {
            let _ = worker.join();
        })
        .ok();
}

/// One evaluation, on the worker thread.
///
/// A panic in here is contained and reported as `worker-exit`: the evaluator
/// is this crate's code, so a panic is a bug in it, and the caller learns that
/// the substrate died rather than losing the turn to an unwind.
fn evaluate(
    request: &RunRequest,
    budget: Budget,
    closing: &Abort,
    calls: tokio::sync::mpsc::UnboundedSender<HostCall>,
) -> RunResult {
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
        let mut state = eval::Run::bridged(
            budget,
            stop.clone(),
            &request.bindings,
            Some(bridge(calls.clone())),
        );
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

/// Turn the async side's channel into the synchronous one the evaluator
/// sends on.
///
/// The evaluator is a thread and knows nothing about tokio; the host side is a
/// task. One forwarding thread per run with host-answered members bridges the
/// two, and ends when the run does, because the sender it holds is dropped.
fn bridge(calls: tokio::sync::mpsc::UnboundedSender<HostCall>) -> mpsc::Sender<HostCall> {
    let (worker_side, from_worker) = mpsc::channel::<HostCall>();
    std::thread::Builder::new()
        .name("tetanus-code-bridge".to_string())
        .spawn(move || {
            while let Ok(call) = from_worker.recv() {
                if calls.send(call).is_err() {
                    return;
                }
            }
        })
        .ok();
    worker_side
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
