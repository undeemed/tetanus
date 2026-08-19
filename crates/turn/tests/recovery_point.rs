//! Test Design Specification: `agent/request-error`, the recovery point.
//!
//! Features under test: what the driver does with a failed model request - who
//! is offered it, what that listener is told, and what its answer changes. The
//! policy that decides whether an attempt is worth making is
//! `upstream_retry_policy.rs`; no policy runs here.
//!
//! Approach: the shared offline fixture, with an `llm/stream` listener standing
//! in for a provider that fails, and a hand-written recovery listener in place
//! of an executor. Nothing waits on a clock: the one case about an interrupt
//! orders itself with notifications rather than delays.
//!
//! Environmental needs: none. No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use harness::Harness;
use tokio::sync::Notify;

use tetanus_core::{EffectHandle, EventBus};
use tetanus_session::SessionEvent;
use tetanus_turn::events::{LlmStream, RequestError, RequestErrorAction, RequestFailure};
use tetanus_turn::llm::mock::PROVIDER;
use tetanus_turn::llm::LlmError;
use tetanus_turn::TurnError;

/// TC-RECOVER-1: a listener that answers `Retry` gets the request sent again,
/// and is told which failure it is answering.
///
/// Input: a route whose first provider call fails with 503, and a listener that
/// answers `Retry` to everything.
/// Expected: the turn completes normally; the provider was called once more
/// than it failed; the listener saw exactly one failure, carrying the turn, the
/// step, the route and the failure's stable code, message and absent
/// provider-asked wait; and the journal says nothing about the failed attempt,
/// because recovery is the listener's to record.
#[tokio::test]
async fn a_listener_that_answers_retry_gets_the_request_sent_again() {
    let h = Harness::new("recover-retry").await;
    let (attempts, _flaky) = flaky(h.bus(), 1, 503);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let _recovery = h.bus().on_waterfall::<RequestError, _>(move |ev, _next| {
        recorded.lock().expect("seen").push((
            ev.turn,
            ev.step,
            ev.provider.clone(),
            ev.failure.clone(),
        ));
        Box::pin(async move { Some(RequestErrorAction::Retry) })
    });

    let outcome = h.engine.run_turn("retry me").await.expect("the turn ran");

    assert_eq!(outcome.content, "You said: retry me");
    assert_eq!(
        attempts.load(Ordering::Relaxed),
        3,
        "two steps, plus the attempt that failed"
    );
    assert_eq!(
        seen.lock().expect("seen").as_slice(),
        [(
            1,
            1,
            PROVIDER.to_string(),
            RequestFailure {
                code: "SERVER".to_string(),
                message: "PROVIDER: 503 upstream is down".to_string(),
                provider_retry_after_ms: None,
            }
        )]
    );

    let journal = journal(&h);
    assert_eq!(journal.iter().filter(|e| e.ty == "turn/end").count(), 1);
    assert!(
        !journal
            .iter()
            .any(|e| e.ty.contains("error") || e.ty.contains("retry")),
        "the driver records no recovery of its own"
    );
}

/// TC-RECOVER-2: with nothing listening, the failure still ends the turn.
///
/// Input: the same failing route, and an empty bus.
/// Expected: the turn fails with the provider's words, the provider was asked
/// exactly once, and the point fired once, between the call that failed and the
/// end of the turn.
#[tokio::test]
async fn an_unoccupied_point_leaves_the_failure_alone() {
    let h = Harness::new("recover-unoccupied").await;
    let (attempts, _flaky) = flaky(h.bus(), 1, 503);

    match h.engine.run_turn("no listener").await {
        Err(TurnError::Llm(err)) => assert!(err.to_string().contains("upstream is down")),
        other => panic!("expected the failure to stand, got {other:?}"),
    }

    assert_eq!(attempts.load(Ordering::Relaxed), 1, "asked once, not again");
    assert_eq!(
        h.trace(),
        [
            "turn/start",
            "agent/pre-step",
            "step/start",
            "user/message",
            "system-prompt/assemble",
            "agent/request",
            "llm/stream",
            "agent/request-error",
        ],
        "the point fires once, after the call, and the turn ends there"
    );
}

/// TC-RECOVER-3: an interrupt beats a retry.
///
/// Input: a listener that answers `Retry`, and a caller that cancels the turn
/// while that answer is being given.
/// Expected: the turn fails on the provider failure rather than waiting and
/// trying again, and the provider was asked exactly once.
#[tokio::test]
async fn a_cancelled_turn_does_not_try_again() {
    let h = Harness::new("recover-cancelled").await;
    let (attempts, _flaky) = flaky(h.bus(), 1, 503);
    // Ordering, not timing: the listener holds its answer until the cancel is
    // set, so the case pins the rule and never races the scheduler.
    let failing = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let (reached, released) = (Arc::clone(&failing), Arc::clone(&cancelled));
    let _recovery = h.bus().on_waterfall::<RequestError, _>(move |_ev, _next| {
        let (reached, released) = (Arc::clone(&reached), Arc::clone(&released));
        Box::pin(async move {
            reached.notify_one();
            released.notified().await;
            Some(RequestErrorAction::Retry)
        })
    });

    let interrupt = async {
        failing.notified().await;
        assert!(h.engine.cancel(), "the turn was running");
        cancelled.notify_one();
    };
    let (failed, ()) = tokio::join!(h.engine.run_turn("stop now"), interrupt);

    assert!(matches!(failed, Err(TurnError::Llm(_))), "{failed:?}");
    assert_eq!(attempts.load(Ordering::Relaxed), 1, "asked once, not again");
}

/// A provider that fails its first `failures` calls and then works, counting
/// every call it was asked to make.
fn flaky(bus: &EventBus, failures: u32, status: u16) -> (Arc<AtomicU32>, EffectHandle) {
    let attempts = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&attempts);
    let handle = bus.on_waterfall::<LlmStream, _>(move |ev, next| {
        let counted = Arc::clone(&counted);
        Box::pin(async move {
            if counted.fetch_add(1, Ordering::Relaxed) < failures {
                return Err(LlmError::Provider {
                    status,
                    message: "upstream is down".into(),
                });
            }
            next.run(ev).await
        })
    });
    (attempts, handle)
}

/// The journal read back off disk rather than out of the log's memory: what a
/// surface or a resumed session sees is the file.
fn journal(h: &Harness) -> Vec<SessionEvent> {
    tetanus_session::replay(&h.log_path).expect("the journal reads back")
}
