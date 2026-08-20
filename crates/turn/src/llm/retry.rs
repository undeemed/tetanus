//! The retry policy a provider route owns: which model-request failures are
//! worth another attempt, and how long to wait before making it.
//!
//! The policy is a value that decides, not a loop that waits. The caller does
//! the waiting and calls the provider again. That split is what keeps every
//! case offline and free of a clock: the delay is returned rather than slept,
//! and jitter is one number the caller hands in.
//!
//! [`install`] is the caller that runs it against a live route: a listener on
//! `agent/request-error` that answers a failed request with another attempt,
//! and records each scheduled retry on the journal before it waits. That wait
//! is cut short when the turn is interrupted, and then the failure stands.
//!
//! Parity: upstream `packages/llm/llm/src/retry-policy.ts` for the shape and
//! the defaults, and `packages/llm/llm-retry/src/index.ts` for the executor.
//! Resolving a policy out of a settings document is not here either, because
//! this crate reads no settings: `tetanus_engine::retry` does it, and hands
//! the result to [`install`].

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tetanus_core::{EffectHandle, EventBus};
use tetanus_session::SessionLog;

use crate::events::{RequestError, RequestErrorAction};

/// Retries allowed after the first attempt, in `normal` mode.
pub const DEFAULT_MAX_RETRIES: u32 = 2;
/// The first local wait, in milliseconds.
pub const DEFAULT_INITIAL_DELAY_MS: f64 = 500.0;
/// The ceiling on any wait, local or provider-asked, in milliseconds.
pub const DEFAULT_MAX_DELAY_MS: f64 = 10_000.0;
/// The symmetric random spread around each local wait.
pub const DEFAULT_JITTER_RATIO: f64 = 0.1;

/// The failure codes `normal` mode retries by default: everything upstream
/// classes as transient. Each one is a code some [`crate::llm::LlmError`]
/// answers, including `EMPTY_RESPONSE`.
pub const DEFAULT_RETRYABLE_CODES: [&str; 5] = [
    "EMPTY_RESPONSE",
    "RATE_LIMIT",
    "SERVER",
    "TIMEOUT",
    "TRANSPORT",
];

/// Bounded exponential backoff with symmetric jitter around each local wait.
///
/// The fields are ranges, not free numbers: `initial_delay_ms` and
/// `max_delay_ms` are positive with the first no larger than the second, and
/// `jitter_ratio` is between zero and one. A value outside those ranges cannot
/// panic - it is clamped when the wait is computed - but it is not meaningful.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backoff {
    pub initial_delay_ms: f64,
    pub max_delay_ms: f64,
    pub jitter_ratio: f64,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial_delay_ms: DEFAULT_INITIAL_DELAY_MS,
            max_delay_ms: DEFAULT_MAX_DELAY_MS,
            jitter_ratio: DEFAULT_JITTER_RATIO,
        }
    }
}

impl Backoff {
    /// The wait before retry number `retry`, counting from one.
    ///
    /// The base doubles per retry and is capped; jitter is a symmetric
    /// multiplier around one, so `random` at zero gives the shortest wait the
    /// ratio allows and `random` at one gives the longest. The cap is applied
    /// again after jitter, so no wait ever exceeds it.
    pub fn local_delay(&self, retry: u32, random: f64) -> Duration {
        // The exponent is bounded because doubling past this is already
        // infinite in floating point, and the cap decides the answer anyway.
        let exponent = retry.saturating_sub(1).min(1024) as i32;
        let exponential = (self.initial_delay_ms * 2f64.powi(exponent)).min(self.max_delay_ms);
        let ratio = self.jitter_ratio.clamp(0.0, 1.0);
        let jitter = 1.0 - ratio + 2.0 * ratio * random.clamp(0.0, 1.0);
        millis((exponential * jitter).min(self.max_delay_ms))
    }
}

/// What a route does with a failed model request.
#[derive(Debug, Clone, PartialEq)]
pub enum RetryPolicy {
    /// Retry the listed failure codes, a bounded number of times.
    Normal {
        max_retries: u32,
        retryable_codes: Vec<String>,
        backoff: Backoff,
    },
    /// Retry every failure until it succeeds or the caller stops asking.
    Always { backoff: Backoff },
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::Normal {
            max_retries: DEFAULT_MAX_RETRIES,
            retryable_codes: DEFAULT_RETRYABLE_CODES
                .iter()
                .map(|c| c.to_string())
                .collect(),
            backoff: Backoff::default(),
        }
    }
}

/// The answer to "that attempt failed - now what?".
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RetryDecision {
    /// The failure stands. The caller reports it.
    GiveUp,
    /// Wait, then make attempt number `retry` of this chain.
    Wait { retry: u32, delay: Duration },
}

impl RetryPolicy {
    pub fn backoff(&self) -> &Backoff {
        match self {
            RetryPolicy::Normal { backoff, .. } | RetryPolicy::Always { backoff } => backoff,
        }
    }

    /// Decide what follows one failed attempt.
    ///
    /// `retries` is how many retries this chain has already made, `code` is the
    /// stable failure code ([`crate::llm::LlmError::code`]),
    /// `provider_retry_after_ms` is the wait the provider asked for when it
    /// asked for one, and `random` is one sample in the zero-to-one range that
    /// jitter is drawn from.
    ///
    /// A provider that asks for longer than the cap is refused rather than
    /// obeyed: bounded mode gives up, because waiting past its own ceiling is
    /// not a bound, and unbounded mode falls back to its local backoff.
    pub fn decide(
        &self,
        retries: u32,
        code: &str,
        provider_retry_after_ms: Option<f64>,
        random: f64,
    ) -> RetryDecision {
        let backoff = self.backoff();
        if let RetryPolicy::Normal {
            max_retries,
            retryable_codes,
            ..
        } = self
        {
            if !retryable_codes.iter().any(|listed| listed == code) {
                return RetryDecision::GiveUp;
            }
            if retries >= *max_retries {
                return RetryDecision::GiveUp;
            }
        }

        let retry = retries + 1;
        let asked = provider_retry_after_ms.filter(|ms| ms.is_finite() && *ms > 0.0);
        let delay = match asked {
            Some(ms) if ms > backoff.max_delay_ms => match self {
                RetryPolicy::Normal { .. } => return RetryDecision::GiveUp,
                RetryPolicy::Always { .. } => backoff.local_delay(retry, random),
            },
            Some(ms) => millis(ms),
            None => backoff.local_delay(retry, random),
        };
        RetryDecision::Wait { retry, delay }
    }
}

/// The durable record of a scheduled retry, written before the wait
/// (`docs/interface-contract.md` section 4.3.2).
pub const RETRY_EVENT: &str = "llm/retry";
/// The durable record that the wait is over and the request is going out
/// again (section 4.3.2).
pub const RETRY_STARTED_EVENT: &str = "llm/retry-started";

/// Where jitter comes from: one sample in the zero-to-one range per decision.
pub type Jitter = Arc<dyn Fn() -> f64 + Send + Sync>;

/// The jitter source a route uses when the caller has no opinion.
///
/// Jitter exists to spread the retries of many callers apart, not to be
/// unguessable, so the low bits of the clock are enough and the workspace
/// stays free of a random-number dependency.
pub fn clock_jitter() -> Jitter {
    Arc::new(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        f64::from(nanos % 1_000_000) / 1_000_000.0
    })
}

/// Run `policy` for one provider route: a failed request on that route is
/// retried, and every scheduled retry is durable before its wait.
///
/// Scoped to a route because a policy belongs to a provider and not to the
/// engine - upstream reads it from each provider's own configuration - so a
/// failure from another route is delegated on untouched. The policy comes
/// from the composer; `tetanus_engine::retry` resolves one out of the settings
/// document for it.
///
/// The returned handle removes the listener, as every registration does.
pub fn install(
    bus: &EventBus,
    log: Arc<dyn SessionLog>,
    provider: impl Into<String>,
    policy: RetryPolicy,
    jitter: Jitter,
) -> EffectHandle {
    let route = provider.into();
    bus.on_waterfall::<RequestError, _>(move |ev, next| {
        let log = Arc::clone(&log);
        let route = route.clone();
        let policy = policy.clone();
        let jitter = Arc::clone(&jitter);
        Box::pin(async move {
            if ev.provider != route {
                return next.run(ev).await;
            }
            // A turn that has already been asked to stop gets no retry, and
            // no record of one: an `llm/retry` entry is a promise to try
            // again, and this listener is not going to.
            if ev.interrupt.stopped() {
                return next.run(ev).await;
            }
            let Some((retry, delay)) = schedule(log.as_ref(), &policy, ev, jitter()) else {
                return next.run(ev).await;
            };
            // The wait is the one part of a turn an interrupt cuts short: a
            // caller who has just asked the turn to stop does not want to
            // sit through a backoff before it does. A cut wait answers no
            // retry, so the failure the driver already has stands.
            if !ev.interrupt.wait(delay).await {
                return next.run(ev).await;
            }
            let started = serde_json::json!({ "turn": ev.turn, "step": ev.step, "retry": retry });
            if let Err(refused) = log.append(RETRY_STARTED_EVENT, started) {
                // The journal is the count. An attempt it cannot record would
                // be invisible to the next decision, which would then allow
                // one retry too many, so the failure stands instead.
                tracing::warn!(%refused, "the journal refused a retry record; not retrying");
                return next.run(ev).await;
            }
            Some(RequestErrorAction::Retry)
        })
    })
}

/// Decide the next attempt and make it durable, or answer `None` when this
/// failure is not one to retry.
fn schedule(
    log: &dyn SessionLog,
    policy: &RetryPolicy,
    ev: &RequestError,
    jitter: f64,
) -> Option<(u32, Duration)> {
    let RetryDecision::Wait { retry, delay } = policy.decide(
        prior_retries(log, ev),
        &ev.failure.code,
        ev.failure.provider_retry_after_ms,
        jitter,
    ) else {
        return None;
    };
    let record = serde_json::json!({
        "turn": ev.turn,
        "step": ev.step,
        "provider": ev.provider,
        "code": ev.failure.code,
        "message": ev.failure.message,
        "retry": retry,
        // An unbounded policy has no ceiling to report, and says so rather
        // than reporting a number a reader would take for a limit.
        "max_retries": match policy {
            RetryPolicy::Normal { max_retries, .. } => serde_json::json!(max_retries),
            RetryPolicy::Always { .. } => serde_json::Value::Null,
        },
        "delay_ms": delay.as_millis() as u64,
    });
    match log.append(RETRY_EVENT, record) {
        Ok(_) => Some((retry, delay)),
        Err(refused) => {
            tracing::warn!(%refused, "the journal refused a retry record; not retrying");
            None
        }
    }
}

/// How many retries this step has already spent on this route, read back from
/// the journal.
///
/// The count is durable rather than kept in the listener, so a resumed session
/// continues the chain instead of starting a new one, and the record a reader
/// sees is the record the policy counted. Upstream counts the same way, from
/// its own `llm/retry` entries.
fn prior_retries(log: &dyn SessionLog, ev: &RequestError) -> u32 {
    log.events()
        .iter()
        .rev()
        .find(|event| {
            event.ty == RETRY_EVENT
                && event.data["turn"] == serde_json::json!(ev.turn)
                && event.data["step"] == serde_json::json!(ev.step)
                && event.data["provider"] == serde_json::json!(ev.provider)
        })
        .and_then(|event| event.data["retry"].as_u64())
        .unwrap_or(0) as u32
}

/// A duration from a millisecond count that may be anything at all: a negative
/// or absent number is no wait, not a panic.
fn millis(ms: f64) -> Duration {
    Duration::from_secs_f64((ms / 1000.0).max(0.0))
}
