//! Holding on to hook runs that nobody is waiting for.
//!
//! Most hook points block the thing they hook: a `PreToolUse` hook decides
//! whether the call runs, so the turn waits for it. Some do not — a
//! notification hook is fired and forgotten. Those runs still own a process,
//! and they still have a continuation that appends to the journal.
//!
//! Which makes them a shutdown problem. If an adapter is disposed while a
//! detached hook is running, three things can go wrong: the process outlives
//! the harness, the continuation writes to a journal that is closing, or
//! shutdown blocks for the hook's full timeout — ten minutes by default.
//!
//! [`DetachedRuns`] is the answer to all three. Every detached run is tracked,
//! shares one [`CancelSignal`], and [`DetachedRuns::drain`] fires that signal
//! before waiting: a still-running hook is *killed* rather than waited out,
//! and the wait is for the continuations, not the processes.
//!
//! Parity: upstream `packages/hooks/hook-protocol/src/detached.ts`, pinned by
//! its `detached.spec.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Why a drain cancelled the runs it was holding.
const DISPOSED: &str = "hook bridge disposed";

#[derive(Debug, Default)]
struct CancelState {
    cancelled: AtomicBool,
    reason: Mutex<Option<String>>,
    woken: Notify,
}

/// A shared "stop now" that a run can be handed and a disposer can fire.
///
/// Cheap to clone; every clone observes the same state. This exists rather
/// than a dependency on a cancellation crate because the whole of what is
/// needed is one flag, one reason and one wakeup.
#[derive(Debug, Clone, Default)]
pub struct CancelSignal(Arc<CancelState>);

impl CancelSignal {
    /// A signal nobody has fired.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether it has been fired.
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    /// Why it was fired, if it was.
    pub fn reason(&self) -> Option<String> {
        self.0.reason.lock().ok()?.clone()
    }

    /// Fire it. Firing twice keeps the first reason: the first cause is the
    /// one worth reporting, and a later one is a consequence.
    pub fn cancel(&self, reason: &str) {
        if let Ok(mut held) = self.0.reason.lock() {
            held.get_or_insert_with(|| reason.to_owned());
        }
        self.0.cancelled.store(true, Ordering::Release);
        self.0.woken.notify_waiters();
    }

    /// Wait until it is fired. Returns at once if it already has been.
    pub async fn cancelled(&self) {
        while !self.is_cancelled() {
            // Registering before the re-check closes the window where a cancel
            // lands between the check and the wait.
            let waiting = self.0.woken.notified();
            if self.is_cancelled() {
                return;
            }
            waiting.await;
        }
    }
}

/// Every detached hook run one adapter has started and not yet seen finish.
#[derive(Debug, Default)]
pub struct DetachedRuns {
    signal: CancelSignal,
    inflight: Mutex<Vec<JoinHandle<()>>>,
}

impl DetachedRuns {
    /// A tracker, one per adapter.
    pub fn new() -> Self {
        Self::default()
    }

    /// The signal every tracked run must be given, so a drain can kill it.
    pub fn signal(&self) -> CancelSignal {
        self.signal.clone()
    }

    /// Hold one detached run until it settles.
    ///
    /// Track the *whole* chain — the hook run and whatever it does afterwards —
    /// so a drain waits for the side effects and not merely for the process to
    /// exit. A run that panics is absorbed here, because this is settlement
    /// bookkeeping; reporting the failure is still the caller's job.
    pub fn track(&self, run: JoinHandle<()>) {
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.push(run);
        }
    }

    /// Cancel every tracked run, then wait for all of them to settle.
    ///
    /// The order matters: cancelling first is what makes this bounded. Waiting
    /// first would block shutdown for the longest hook timeout still running.
    ///
    /// The loop re-checks rather than waiting on one snapshot, because a run's
    /// continuation can track another run while the first wave is settling. A
    /// run tracked *after* this returns is nobody's responsibility — by then
    /// the adapter's listeners are gone and nothing can start one.
    pub async fn drain(&self) {
        self.signal.cancel(DISPOSED);
        loop {
            let wave: Vec<JoinHandle<()>> = match self.inflight.lock() {
                Ok(mut inflight) => std::mem::take(&mut *inflight),
                Err(_) => return,
            };
            if wave.is_empty() {
                return;
            }
            for run in wave {
                // A panicking run is a settled run. Its failure is the
                // caller's to report, and a drain that propagated it would
                // abandon the runs it had not waited for yet.
                let _ = run.await;
            }
        }
    }
}
