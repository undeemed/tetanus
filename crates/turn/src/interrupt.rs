//! The interrupt a running turn watches: one flag every part of the turn can
//! read, and wait on.
//!
//! A turn is interrupted between steps, never in the middle of a provider
//! call, so the journal stays a record of what actually happened. The one
//! thing worth waking early is a wait the turn is only doing to be polite -
//! the retry executor's backoff - because a caller who has just asked the
//! turn to stop should not sit through ten seconds of it first.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

/// Set once per turn, read by everything that turn runs.
///
/// The state lives in a `watch` channel rather than an atomic and a notify
/// pair, because a waiter must not be able to miss the interrupt that arrived
/// while it was starting to wait: a `watch` receiver sees a value that changed
/// before it began.
pub struct Interrupt {
    stopped: watch::Sender<bool>,
}

impl Default for Interrupt {
    fn default() -> Self {
        Self {
            stopped: watch::channel(false).0,
        }
    }
}

impl Interrupt {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Ask the turn to stop. Answers `true` when this call is what stopped it,
    /// and `false` when it was already asked.
    pub fn stop(&self) -> bool {
        !self.stopped.send_replace(true)
    }

    /// Whether the turn has been asked to stop.
    pub fn stopped(&self) -> bool {
        *self.stopped.borrow()
    }

    /// Forget an interrupt that stopped nothing. A turn clears the flag as it
    /// starts, so an interrupt that arrived while the session was idle does
    /// not stop the turn that follows it.
    pub fn clear(&self) {
        self.stopped.send_replace(false);
    }

    /// Resolve when the turn is interrupted, and never otherwise.
    ///
    /// For a caller that is racing the interrupt against work rather than
    /// against a clock - an outstanding question, say - where
    /// [`wait`](Self::wait) would need a delay it does not have. Like `wait`,
    /// it subscribes before it checks, so an interrupt that landed while the
    /// race was being set up is not missed.
    pub async fn cancelled(&self) {
        let mut stopped = self.stopped.subscribe();
        if *stopped.borrow_and_update() {
            return;
        }
        // The sender outlives every turn that watches it, so the only way this
        // resolves is the value changing to `true`.
        let _ = stopped.changed().await;
    }

    /// Wait for `delay`, or until the turn is interrupted, whichever is first.
    ///
    /// Answers `true` when the wait finished, and `false` when the interrupt
    /// cut it short - so a caller reads the answer as "carry on" or "stop".
    pub async fn wait(&self, delay: Duration) -> bool {
        let mut stopped = self.stopped.subscribe();
        if *stopped.borrow_and_update() {
            return false;
        }
        tokio::select! {
            _ = stopped.changed() => false,
            _ = tokio::time::sleep(delay) => true,
        }
    }
}
