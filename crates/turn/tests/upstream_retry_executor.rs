//! Test Design Specification: the retry policy run against a live route.
//!
//! Features under test: the executor half of upstream
//! `packages/llm/llm-retry/tests/retry.spec.ts` - which failed model requests
//! are sent again, how many times, and what the journal says about each wait.
//! The decision half is `upstream_retry_policy.rs`; it is not restated here.
//!
//! Approach: the shared offline fixture, with an `llm/stream` listener standing
//! in for a provider that fails. Waits are real but measured in single
//! milliseconds, because the numbers a policy computes are already pinned by
//! the decision suite; what these cases pin is that the executor serves the
//! decision it was given and records it.
//!
//! Environmental needs: none. No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use harness::Harness;
use tetanus_core::{EffectHandle, EventBus};
use tetanus_session::SessionEvent;
use tetanus_turn::events::LlmStream;
use tetanus_turn::llm::mock::PROVIDER;
use tetanus_turn::llm::retry::{
    install, Backoff, Jitter, RetryPolicy, RETRY_EVENT, RETRY_STARTED_EVENT,
};
use tetanus_turn::llm::LlmError;
use tetanus_turn::TurnError;

/// TC-PORT-RETRYX-1: a retryable failure is sent again, and the journal says
/// so before the wait.
///
/// Upstream: `retry.spec.ts`, "retries a retryable failure and records the
/// scheduled attempt".
///
/// Input: a route whose first provider call fails with 503, under a normal
/// policy that retries `SERVER` twice.
/// Expected: the turn completes normally; the provider was called once more
/// than it failed; the journal carries one `llm/retry` with every key section
/// 4.3.2 fixes, then one `llm/retry-started` naming the same attempt.
#[tokio::test]
async fn a_retryable_failure_is_sent_again_and_recorded() {
    let h = Harness::new("retryx-recorded").await;
    let (attempts, _flaky) = flaky(h.bus(), 1, 503);
    let _executor = install(
        h.bus(),
        Arc::clone(h.engine.log()),
        PROVIDER,
        normal(2),
        fixed_jitter(),
    );

    let outcome = h.engine.run_turn("retry me").await.expect("the turn ran");

    assert_eq!(outcome.content, "You said: retry me");
    assert_eq!(
        attempts.load(Ordering::Relaxed),
        3,
        "two steps, plus the attempt that failed"
    );

    let scheduled = records(&h, RETRY_EVENT);
    assert_eq!(scheduled.len(), 1, "one failure, one scheduled retry");
    let data = &scheduled[0].data;
    assert_eq!(data["turn"], 1);
    assert_eq!(data["step"], 1);
    assert_eq!(data["provider"], PROVIDER);
    assert_eq!(data["code"], "SERVER");
    assert_eq!(data["message"], "PROVIDER: 503 upstream is down");
    assert_eq!(data["retry"], 1, "the attempt about to be made");
    assert_eq!(data["max_retries"], 2);
    assert_eq!(data["delay_ms"], 2, "the first local wait, jitter fixed");

    let started = records(&h, RETRY_STARTED_EVENT);
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].data["retry"], 1, "the wait is over");
    assert!(
        started[0].seq > scheduled[0].seq,
        "the schedule is durable before the wait, not after it"
    );

    // The same sequence a reader of the turn sees: the call, the recovery
    // point, the two records that bracket the wait, then the call again.
    let trace = h.trace();
    let at = trace
        .iter()
        .position(|topic| topic == "agent/request-error")
        .expect("the recovery point fired");
    assert_eq!(
        trace[at - 1..at + 4],
        [
            "llm/stream",
            "agent/request-error",
            RETRY_EVENT,
            RETRY_STARTED_EVENT,
            "llm/stream"
        ]
    );
}

/// TC-PORT-RETRYX-2: a failure the policy does not list stands.
///
/// Upstream: `retry.spec.ts`, "does not retry a code outside retryableCodes".
///
/// Input: the same route failing with 400, which classes as `PROVIDER`, under
/// a policy that lists only `SERVER`.
/// Expected: the turn fails with that provider message, the provider was called
/// exactly once, and nothing was recorded.
#[tokio::test]
async fn a_code_the_policy_does_not_list_is_not_retried() {
    let h = Harness::new("retryx-unlisted").await;
    let (attempts, _flaky) = flaky(h.bus(), 1, 400);
    let _executor = install(
        h.bus(),
        Arc::clone(h.engine.log()),
        PROVIDER,
        normal(2),
        fixed_jitter(),
    );

    match h.engine.run_turn("no retry").await {
        Err(TurnError::Llm(err)) => assert!(err.to_string().contains("upstream is down")),
        other => panic!("expected the failure to stand, got {other:?}"),
    }
    assert_eq!(attempts.load(Ordering::Relaxed), 1, "asked once, not again");
    assert!(records(&h, RETRY_EVENT).is_empty());
}

/// TC-PORT-RETRYX-3: the bound is a bound.
///
/// Upstream: `retry.spec.ts`, "stops after maxRetries attempts".
///
/// Input: a route that fails every call with 503, under a policy that allows
/// two retries.
/// Expected: three attempts in all, two scheduled records numbered one and two,
/// and the turn ends on the provider failure.
#[tokio::test]
async fn a_route_that_never_recovers_stops_at_the_bound() {
    let h = Harness::new("retryx-bound").await;
    let (attempts, _flaky) = flaky(h.bus(), u32::MAX, 503);
    let _executor = install(
        h.bus(),
        Arc::clone(h.engine.log()),
        PROVIDER,
        normal(2),
        fixed_jitter(),
    );

    let failed = h.engine.run_turn("never works").await;

    assert!(matches!(failed, Err(TurnError::Llm(_))), "{failed:?}");
    assert_eq!(
        attempts.load(Ordering::Relaxed),
        3,
        "the first attempt plus the two retries the bound allows"
    );
    let numbers: Vec<serde_json::Value> = records(&h, RETRY_EVENT)
        .into_iter()
        .map(|event| event.data["retry"].clone())
        .collect();
    assert_eq!(numbers, [serde_json::json!(1), serde_json::json!(2)]);
}

/// TC-PORT-RETRYX-4: an unbounded policy retries what a normal one refuses,
/// and reports no ceiling.
///
/// Upstream: `retry.spec.ts`, "always mode retries any failure".
///
/// Input: two failures with 400 - a code outside every default set - under
/// `RetryPolicy::Always`.
/// Expected: the turn completes; both retries were made; each record carries
/// `max_retries: null` rather than a number a reader would take for a limit.
#[tokio::test]
async fn an_unbounded_policy_retries_a_code_no_set_lists() {
    let h = Harness::new("retryx-always").await;
    let (attempts, _flaky) = flaky(h.bus(), 2, 400);
    let _executor = install(
        h.bus(),
        Arc::clone(h.engine.log()),
        PROVIDER,
        RetryPolicy::Always { backoff: quick() },
        fixed_jitter(),
    );

    let outcome = h
        .engine
        .run_turn("keep trying")
        .await
        .expect("the turn ran");

    assert_eq!(outcome.content, "You said: keep trying");
    assert_eq!(attempts.load(Ordering::Relaxed), 4, "two failed, two steps");
    for record in records(&h, RETRY_EVENT) {
        assert_eq!(
            record.data["max_retries"],
            serde_json::Value::Null,
            "an unbounded policy has no ceiling to report"
        );
    }
}

/// TC-PORT-RETRYX-5: another route's failure is not this policy's business.
///
/// Upstream: `retry.spec.ts`, "leaves a provider with no policy to the next
/// handler".
///
/// Input: the executor installed for a route the engine does not use, and a
/// 503 that the policy would otherwise retry.
/// Expected: one attempt, no records, and the failure ends the turn.
#[tokio::test]
async fn a_failure_from_another_route_is_delegated() {
    let h = Harness::new("retryx-other-route").await;
    let (attempts, _flaky) = flaky(h.bus(), 1, 503);
    let _executor = install(
        h.bus(),
        Arc::clone(h.engine.log()),
        "some-other-provider",
        normal(2),
        fixed_jitter(),
    );

    let failed = h.engine.run_turn("not my route").await;

    assert!(matches!(failed, Err(TurnError::Llm(_))), "{failed:?}");
    assert_eq!(attempts.load(Ordering::Relaxed), 1);
    assert!(records(&h, RETRY_EVENT).is_empty());
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

/// Waits short enough that a case does not measure the clock: two milliseconds
/// for the first retry, doubling under the same cap the policy applies.
fn quick() -> Backoff {
    Backoff {
        initial_delay_ms: 2.0,
        max_delay_ms: 8.0,
        jitter_ratio: 0.0,
    }
}

fn normal(max_retries: u32) -> RetryPolicy {
    RetryPolicy::Normal {
        max_retries,
        retryable_codes: vec!["SERVER".to_string()],
        backoff: quick(),
    }
}

/// A jitter source that samples the middle of the range, so a case asserts one
/// delay rather than a band. The spread itself is pinned by the decision suite.
fn fixed_jitter() -> Jitter {
    Arc::new(|| 0.5)
}

/// The records of one type, read back off the journal rather than out of the
/// log's memory: what a surface or a resumed session sees is the file.
fn records(h: &Harness, ty: &str) -> Vec<SessionEvent> {
    tetanus_session::replay(&h.log_path)
        .expect("the journal reads back")
        .into_iter()
        .filter(|event| event.ty == ty)
        .collect()
}
