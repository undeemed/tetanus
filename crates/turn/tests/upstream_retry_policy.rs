//! Test Design Specification: the provider retry policy, ported.
//!
//! Feature under test: [`RetryPolicy`] - which failed model requests are worth
//! another attempt, and the wait before it. Upstream pins the same rules in
//! `packages/llm/llm/tests/retry-policy.spec.ts` (the defaults) and
//! `packages/llm/llm-retry/tests/retry.spec.ts` (the decision); each case names
//! the upstream behaviour it comes from.
//!
//! Approach: the policy alone. It returns a wait rather than taking one, so no
//! case sleeps and no case reads a clock, and jitter is a number the case
//! chooses instead of a random sample. Upstream's configuration-validation
//! cases are not restated: tetanus resolves no policy out of settings yet, and
//! the ones that survive its types would be a contract with no consumer. That
//! gap is a row in `docs/parity.md`.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::time::Duration;

use tetanus_turn::llm::retry::{Backoff, RetryDecision, RetryPolicy};
use tetanus_turn::llm::LlmError;

/// TC-PORT-RETRY-1: the default policy is upstream's default policy.
///
/// Upstream: `retry-policy.spec.ts`, "resolves immutable normal defaults".
///
/// Expected: bounded mode, two retries, the five transient codes, and a
/// 500 ms first wait capped at 10 s with a tenth of jitter.
#[test]
fn the_default_policy_is_bounded_with_the_documented_numbers() {
    let RetryPolicy::Normal {
        max_retries,
        retryable_codes,
        backoff,
    } = RetryPolicy::default()
    else {
        panic!("the default is bounded");
    };

    let transient = [
        "EMPTY_RESPONSE",
        "RATE_LIMIT",
        "SERVER",
        "TIMEOUT",
        "TRANSPORT",
    ];

    assert_eq!(max_retries, 2);
    assert_eq!(retryable_codes, transient);
    assert_eq!(backoff.initial_delay_ms, 500.0);
    assert_eq!(backoff.max_delay_ms, 10_000.0);
    assert_eq!(backoff.jitter_ratio, 0.1);
}

/// TC-PORT-RETRY-2: bounded mode retries a listed code and nothing else.
///
/// Upstream: `retry.spec.ts`, "retries a configured transient code" and "leaves
/// an unlisted code to the caller".
///
/// Input: a policy listing only `SERVER`, asked about `SERVER` and about
/// `PROTOCOL`.
/// Expected: the first schedules retry one, the second gives up. A failure the
/// policy was not told about is not transient by default.
#[test]
fn bounded_mode_retries_only_a_listed_code() {
    let policy = bounded(2);

    assert_eq!(policy.decide(0, "SERVER", None, 0.0), wait(1, 500));
    assert_eq!(
        policy.decide(0, "PROTOCOL", None, 0.0),
        RetryDecision::GiveUp
    );
}

/// TC-PORT-RETRY-3: bounded mode stops at its cap.
///
/// Upstream: `retry.spec.ts`, "stops after maxRetries attempts".
///
/// Input: a cap of two, asked after zero, one and two retries.
/// Expected: retry one, retry two, then give up. The cap counts retries after
/// the first request, so a cap of two makes three attempts in all.
#[test]
fn bounded_mode_stops_at_its_cap() {
    let policy = bounded(2);

    assert!(matches!(
        policy.decide(0, "SERVER", None, 0.0),
        RetryDecision::Wait { retry: 1, .. }
    ));
    assert!(matches!(
        policy.decide(1, "SERVER", None, 0.0),
        RetryDecision::Wait { retry: 2, .. }
    ));
    assert_eq!(policy.decide(2, "SERVER", None, 0.0), RetryDecision::GiveUp);
}

/// TC-PORT-RETRY-4: the local wait doubles, and the cap holds it.
///
/// Upstream: `retry.spec.ts`, "applies bounded exponential backoff".
///
/// Input: no jitter, a 500 ms start and a 3 s cap, over four retries.
/// Expected: 500, 1000, 2000, then 3000 - the cap, not 4000. A long outage
/// cannot make the wait grow without bound.
#[test]
fn the_local_wait_doubles_up_to_the_cap() {
    let backoff = Backoff {
        initial_delay_ms: 500.0,
        max_delay_ms: 3_000.0,
        jitter_ratio: 0.0,
    };

    let waits: Vec<f64> = (1..=4)
        .map(|retry| ms(backoff.local_delay(retry, 0.5)))
        .collect();

    assert_eq!(waits, [500.0, 1_000.0, 2_000.0, 3_000.0]);
}

/// TC-PORT-RETRY-5: jitter is symmetric around the base wait.
///
/// Upstream: `retry.spec.ts`, "spreads each delay symmetrically by
/// jitterRatio".
///
/// Input: a 1 s base with a fifth of jitter, sampled at zero, a half and one;
/// then the same at the cap.
/// Expected: 800, 1000 and 1200 ms. At the cap the upper half is cut off, so
/// jitter can shorten a capped wait but never lengthen it past the cap.
#[test]
fn jitter_spreads_each_wait_symmetrically_and_never_past_the_cap() {
    let backoff = Backoff {
        initial_delay_ms: 1_000.0,
        max_delay_ms: 1_000.0,
        jitter_ratio: 0.2,
    };

    assert_close(ms(backoff.local_delay(1, 0.0)), 800.0);
    assert_close(ms(backoff.local_delay(1, 0.5)), 1_000.0);
    assert_close(ms(backoff.local_delay(1, 1.0)), 1_000.0);

    let uncapped = Backoff {
        max_delay_ms: 10_000.0,
        ..backoff
    };
    assert_close(ms(uncapped.local_delay(1, 1.0)), 1_200.0);
}

/// TC-PORT-RETRY-6: a provider that asks for a wait inside the cap is obeyed.
///
/// Upstream: `retry.spec.ts`, "honours providerRetryAfterMs when it is within
/// maxDelayMs".
///
/// Input: a rate limit asking for 2 s under a 10 s cap.
/// Expected: exactly 2 s, not the local backoff. A provider knows when it will
/// serve the request again; guessing is only for when it does not say.
#[test]
fn a_provider_wait_inside_the_cap_is_used_as_asked() {
    let policy = bounded(2);

    assert_eq!(
        policy.decide(0, "SERVER", Some(2_000.0), 0.0),
        wait(1, 2_000)
    );
}

/// TC-PORT-RETRY-7: a provider that asks for longer than the cap is refused.
///
/// Upstream: `retry.spec.ts`, "declines a providerRetryAfterMs beyond
/// maxDelayMs under a bounded policy, and falls back locally under always".
///
/// Input: a 30 s ask under a 10 s cap, put to both modes.
/// Expected: bounded gives up, because waiting past its own ceiling is not a
/// bound; unbounded ignores the ask and waits its local backoff instead.
#[test]
fn a_provider_wait_past_the_cap_is_refused_by_each_mode_in_its_own_way() {
    assert_eq!(
        bounded(2).decide(0, "SERVER", Some(30_000.0), 0.0),
        RetryDecision::GiveUp
    );
    assert_eq!(
        unbounded().decide(0, "SERVER", Some(30_000.0), 0.0),
        wait(1, 500)
    );
}

/// TC-PORT-RETRY-8: unbounded mode retries any code, without a cap.
///
/// Upstream: `retry-policy.spec.ts`, "resolves always mode with default
/// backoff", and `retry.spec.ts`, "always mode retries every failure".
///
/// Input: a code no bounded policy lists, after a hundred retries.
/// Expected: retry 101, at the capped wait. Unbounded means the caller decides
/// when to stop, not the policy.
#[test]
fn unbounded_mode_retries_any_code_without_a_cap() {
    assert_eq!(
        unbounded().decide(100, "PROTOCOL", None, 0.0),
        wait(101, 10_000)
    );
}

/// TC-PORT-RETRY-9: every provider failure carries the code a policy reads.
///
/// Upstream: the `code` field the retry plugin matches against.
///
/// Input: one of each `LlmError`, including three provider statuses.
/// Expected: the printed prefix for everything but a provider response, and
/// the status class for that - so a 429 is a `RATE_LIMIT` the default policy
/// retries, while a 400 is a `PROVIDER` failure it does not.
#[test]
fn every_failure_carries_the_code_a_policy_reads() {
    let provider = |status| {
        LlmError::Provider {
            status,
            message: "..".into(),
        }
        .code()
    };

    assert_eq!(LlmError::Transport("..".into()).code(), "TRANSPORT");
    assert_eq!(LlmError::Protocol("..".into()).code(), "PROTOCOL");
    assert_eq!(LlmError::Sink("..".into()).code(), "SINK");
    assert_eq!(
        LlmError::MissingCredential("k".into()).code(),
        "MISSING_CREDENTIAL"
    );
    assert_eq!(provider(408), "TIMEOUT");
    assert_eq!(provider(429), "RATE_LIMIT");
    assert_eq!(provider(503), "SERVER");
    assert_eq!(provider(400), "PROVIDER");

    let default = RetryPolicy::default();
    assert!(matches!(
        default.decide(0, provider(429), None, 0.0),
        RetryDecision::Wait { .. }
    ));
    assert_eq!(
        default.decide(0, provider(400), None, 0.0),
        RetryDecision::GiveUp
    );
}

/// TC-PORT-RETRY-10: a provider that completed and said nothing is retried.
///
/// Upstream: `retry.spec.ts`, "retries an EMPTY_RESPONSE error finish under the
/// default retryable codes".
///
/// Input: [`LlmError::EmptyResponse`] against the default policy.
/// Expected: its code is `EMPTY_RESPONSE`, which the defaults already list, so
/// the decision is another attempt. Until an adapter could raise this error the
/// listed code was unreachable; the DeepSeek adapter now raises it, so the
/// default retryable set is fully served.
#[test]
fn a_completed_response_with_no_content_is_worth_another_attempt() {
    let empty =
        LlmError::EmptyResponse("model \"m\" returned a completed response with no content".into());

    assert_eq!(empty.code(), "EMPTY_RESPONSE");
    assert_eq!(
        empty.to_string(),
        "EMPTY_RESPONSE: model \"m\" returned a completed response with no content"
    );
    assert!(matches!(
        RetryPolicy::default().decide(0, empty.code(), None, 0.0),
        RetryDecision::Wait { .. }
    ));
}

/// The default waits with jitter turned off, so a case can assert an exact one.
fn steady() -> Backoff {
    Backoff {
        jitter_ratio: 0.0,
        ..Backoff::default()
    }
}

/// A bounded policy that lists two codes and nothing else.
fn bounded(max_retries: u32) -> RetryPolicy {
    RetryPolicy::Normal {
        max_retries,
        retryable_codes: vec!["SERVER".to_string(), "RATE_LIMIT".to_string()],
        backoff: steady(),
    }
}

/// An unbounded policy over those same waits.
fn unbounded() -> RetryPolicy {
    RetryPolicy::Always { backoff: steady() }
}

/// The decision to make attempt number `retry` after `ms` milliseconds.
fn wait(retry: u32, ms: u64) -> RetryDecision {
    RetryDecision::Wait {
        retry,
        delay: Duration::from_millis(ms),
    }
}

fn ms(delay: Duration) -> f64 {
    delay.as_secs_f64() * 1_000.0
}

fn assert_close(seen: f64, expected: f64) {
    assert!(
        (seen - expected).abs() < 1e-6,
        "expected {expected} ms, saw {seen} ms"
    );
}
