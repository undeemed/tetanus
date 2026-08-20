//! Test Design Specification: the interrupt a running turn watches.
//!
//! Features under test: `tetanus_turn::interrupt::Interrupt` - what `stop`
//! answers, what `stopped` reads, what `clear` forgets, and the two ways a
//! wait can end.
//!
//! Approach: the type on its own, with no engine and no journal, because what
//! these cases pin is the flag's contract rather than any use of it. The one
//! case that must pin an ordering uses `tokio::join!`, which polls the wait
//! before the stop, rather than a sleep that hopes for the same order.
//!
//! Features NOT tested here: what a turn does with an interrupt. The retry
//! executor's use of it is `upstream_retry_executor.rs` TC-PORT-RETRYX-6, and
//! the step-boundary check is `recovery_point.rs` TC-RECOVER-3.
//!
//! Environmental needs: none.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::time::{Duration, Instant};

use tetanus_turn::interrupt::Interrupt;

/// A delay no case may actually spend, so a case that returns has been cut
/// short rather than merely served quickly.
const FOREVER: Duration = Duration::from_secs(30);

/// TC-INT-1: a wait nobody interrupts runs to its end.
///
/// Input: a fresh interrupt, and a wait of one millisecond.
/// Expected: the wait answers `true`, and the interrupt still reads as not
/// stopped.
#[tokio::test]
async fn an_uninterrupted_wait_finishes() {
    let interrupt = Interrupt::new();

    assert!(interrupt.wait(Duration::from_millis(1)).await);
    assert!(!interrupt.stopped());
}

/// TC-INT-2: a stop during the wait cuts it short.
///
/// Input: a wait of thirty seconds, and a `stop` from another task while it
/// is waiting.
/// Expected: the wait answers `false` in under five seconds.
#[tokio::test]
async fn a_stop_during_the_wait_ends_it() {
    let interrupt = Interrupt::new();
    let started = Instant::now();

    let stop = async {
        assert!(interrupt.stop(), "this call is what stopped it");
    };
    let (finished, ()) = tokio::join!(interrupt.wait(FOREVER), stop);

    assert!(!finished, "the interrupt ended the wait, not the delay");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it waited it out"
    );
}

/// TC-INT-3: a stop that arrives before the wait is not missed.
///
/// This is why the flag is a watched value and not a notification: a waiter
/// that subscribes after the stop must still see it, or a turn that is asked
/// to stop at exactly the wrong moment waits out its whole backoff.
///
/// Input: `stop`, then a wait of thirty seconds.
/// Expected: the wait answers `false` in under five seconds.
#[tokio::test]
async fn a_stop_before_the_wait_is_not_missed() {
    let interrupt = Interrupt::new();
    interrupt.stop();
    let started = Instant::now();

    assert!(!interrupt.wait(FOREVER).await);
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// TC-INT-4: an interrupt that stopped nothing is forgotten.
///
/// Input: `stop` twice, then `clear`, then a wait of one millisecond.
/// Expected: the first `stop` answers `true` and the second `false`; after
/// `clear` the flag reads as not stopped and the wait finishes.
#[tokio::test]
async fn clearing_forgets_a_stop() {
    let interrupt = Interrupt::new();

    assert!(interrupt.stop(), "the first call is what stopped it");
    assert!(!interrupt.stop(), "the second found it already stopped");
    assert!(interrupt.stopped());

    interrupt.clear();

    assert!(!interrupt.stopped());
    assert!(interrupt.wait(Duration::from_millis(1)).await);
}
