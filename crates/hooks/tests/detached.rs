//! Conformance: hook runs nobody is waiting for.
//!
//! Feature under test: `tetanus_hooks::detached::DetachedRuns` — the tracker
//! that keeps a disposed adapter from leaving a hook process or a late journal
//! append behind it.
//!
//! Ported from upstream `packages/hooks/hook-protocol/tests/detached.spec.ts`.
//! Case ids TC-HOOK-DET-1..8. The last three are this port's own.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tetanus_hooks::detached::{CancelSignal, DetachedRuns};
use tokio::sync::oneshot;

/// Long enough that a drain which is going to resolve early already has.
///
/// Used only for "it should NOT have finished yet" checks. A loaded machine
/// makes those *more* reliable, not less: everything is slower, so an early
/// finish is even less likely.
const A_MOMENT: Duration = Duration::from_millis(50);

/// The bound on "it should finish". Deliberately far longer than the work,
/// because this one fails when the machine is busy rather than when the code
/// is wrong - and six lanes share this machine.
const PATIENCE: Duration = Duration::from_secs(10);

/// TC-HOOK-DET-1: the signal starts unfired, and a drain fires it — so a hook
/// process still running is killed rather than waited out to its timeout.
#[tokio::test]
async fn a_drain_fires_the_signal_it_handed_out() {
    let detached = DetachedRuns::new();
    assert!(!detached.signal().is_cancelled());

    detached.drain().await;

    assert!(detached.signal().is_cancelled());
    assert_eq!(
        detached.signal().reason().as_deref(),
        Some("hook bridge disposed")
    );
}

/// TC-HOOK-DET-2: draining an idle tracker returns at once.
#[tokio::test]
async fn draining_nothing_returns_immediately() {
    tokio::time::timeout(PATIENCE, DetachedRuns::new().drain())
        .await
        .expect("an empty drain must not block");
}

/// TC-HOOK-DET-3: a drain waits for a tracked run to settle.
#[tokio::test]
async fn a_drain_waits_for_a_tracked_run() {
    let detached = Arc::new(DetachedRuns::new());
    let (release, held) = oneshot::channel::<()>();
    detached.track(tokio::spawn(async move {
        let _ = held.await;
    }));

    let draining = {
        let detached = Arc::clone(&detached);
        tokio::spawn(async move { detached.drain().await })
    };

    // Give the drain every chance to wrongly finish before the run settles.
    tokio::time::sleep(A_MOMENT).await;
    assert!(!draining.is_finished(), "the drain finished early");

    release.send(()).expect("release the run");
    tokio::time::timeout(PATIENCE, draining)
        .await
        .expect("the drain should finish once the run settles")
        .expect("join");
}

/// TC-HOOK-DET-4: a run tracked *while a prior wave is settling* is still
/// waited for. This is the case a single snapshot of the registry gets wrong.
#[tokio::test]
async fn a_drain_waits_for_a_run_tracked_during_a_prior_wave() {
    let detached = Arc::new(DetachedRuns::new());
    let (release_first, first) = oneshot::channel::<()>();
    let (release_second, second) = oneshot::channel::<()>();

    // The first run's own continuation tracks the second, after the drain has
    // already taken its first wave.
    let tracker = Arc::clone(&detached);
    detached.track(tokio::spawn(async move {
        let _ = first.await;
        tracker.track(tokio::spawn(async move {
            let _ = second.await;
        }));
    }));

    let draining = {
        let detached = Arc::clone(&detached);
        tokio::spawn(async move { detached.drain().await })
    };

    // Let the drain take its first wave and park on the first run BEFORE that
    // run finishes. Without this the first run completes first, the late run
    // is already in the registry when the wave is taken, and the case passes
    // against a drain that only ever handles one wave.
    tokio::time::sleep(A_MOMENT).await;
    release_first.send(()).expect("release the first run");
    tokio::time::sleep(A_MOMENT).await;
    assert!(
        !draining.is_finished(),
        "the drain stopped at its first wave and missed the late run"
    );

    release_second.send(()).expect("release the second run");
    tokio::time::timeout(PATIENCE, draining)
        .await
        .expect("the drain should finish once the late run settles")
        .expect("join");
}

/// TC-HOOK-DET-5: a run that panicked is a settled run.
///
/// The tracker's bookkeeping must not depend on the caller having attached a
/// handler: a drain that propagated the panic would abandon every run it had
/// not waited for yet, which is the opposite of what a drain is for.
#[tokio::test]
async fn a_panicking_run_is_absorbed_and_the_drain_still_finishes() {
    let detached = DetachedRuns::new();
    detached.track(tokio::spawn(async {
        panic!("hook run boom");
    }));

    tokio::time::timeout(PATIENCE, detached.drain())
        .await
        .expect("a panicking run must not wedge the drain");
}

/// TC-HOOK-DET-6: a run holding the signal is told to stop.
///
/// This port's own, and the point of the whole module: the tracker is only
/// useful if what it hands out actually reaches a running hook. Without this,
/// every other case here would still pass with a signal nobody consults.
#[tokio::test]
async fn a_running_hook_observes_the_cancellation() {
    let detached = Arc::new(DetachedRuns::new());
    let signal = detached.signal();
    let noticed = Arc::new(AtomicBool::new(false));

    let flag = Arc::clone(&noticed);
    detached.track(tokio::spawn(async move {
        signal.cancelled().await;
        flag.store(true, Ordering::Release);
    }));

    // The run is parked on the signal, so only the drain can finish it.
    tokio::time::timeout(PATIENCE, detached.drain())
        .await
        .expect("the drain should cancel and then finish");
    assert!(
        noticed.load(Ordering::Acquire),
        "the run never saw the cancellation"
    );
}

/// TC-HOOK-DET-7: a signal fired twice keeps the first reason.
///
/// This port's own. The first cause is the one worth reporting; a later one is
/// usually a consequence of it, and overwriting would replace the diagnosis
/// with the symptom.
#[tokio::test]
async fn the_first_reason_survives_a_second_cancellation() {
    let signal = CancelSignal::new();
    signal.cancel("the real cause");
    signal.cancel("a later consequence");
    assert_eq!(signal.reason().as_deref(), Some("the real cause"));
}

/// TC-HOOK-DET-8: waiting on an already-fired signal returns at once.
///
/// This port's own. A hook that checks for cancellation after the drain has
/// already run must not park forever waiting for a notification that has been
/// and gone — the window between the check and the wait is exactly where a
/// naive implementation deadlocks, and a wedged run would wedge the drain.
#[tokio::test]
async fn waiting_on_an_already_fired_signal_returns_at_once() {
    let signal = CancelSignal::new();
    signal.cancel("already gone");
    tokio::time::timeout(PATIENCE, signal.cancelled())
        .await
        .expect("an already-fired signal must not park its waiter");
}
