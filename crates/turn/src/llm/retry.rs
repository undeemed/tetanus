//! The retry policy a provider route owns: which model-request failures are
//! worth another attempt, and how long to wait before making it.
//!
//! The policy is a value that decides, not a loop that waits. The caller does
//! the waiting and calls the provider again. That split is what keeps every
//! case offline and free of a clock: the delay is returned rather than slept,
//! and jitter is one number the caller hands in.
//!
//! Parity: upstream `packages/llm/llm/src/retry-policy.ts` for the shape and
//! the defaults, and the decision half of `packages/llm/llm-retry/src/index.ts`
//! for what is retried and for how long. Resolving a policy out of a settings
//! document is not here: nothing reads settings into the turn engine yet, and
//! validating a value no one can configure would be a contract with no
//! consumer. That gap is a row in `docs/parity.md`.

use std::time::Duration;

/// Retries allowed after the first attempt, in `normal` mode.
pub const DEFAULT_MAX_RETRIES: u32 = 2;
/// The first local wait, in milliseconds.
pub const DEFAULT_INITIAL_DELAY_MS: f64 = 500.0;
/// The ceiling on any wait, local or provider-asked, in milliseconds.
pub const DEFAULT_MAX_DELAY_MS: f64 = 10_000.0;
/// The symmetric random spread around each local wait.
pub const DEFAULT_JITTER_RATIO: f64 = 0.1;

/// The failure codes `normal` mode retries by default: everything upstream
/// classes as transient. tetanus has no `EMPTY_RESPONSE` failure yet, and the
/// code is kept in the set so a provider that grows one is retried without a
/// policy change.
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

/// A duration from a millisecond count that may be anything at all: a negative
/// or absent number is no wait, not a panic.
fn millis(ms: f64) -> Duration {
    Duration::from_secs_f64((ms / 1000.0).max(0.0))
}
