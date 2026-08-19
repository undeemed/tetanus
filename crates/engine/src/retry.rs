//! The model-request retry policy, resolved out of the settings document.
//!
//! [`tetanus_turn::llm::retry`] owns the decision and the executor; what was
//! missing was the step before both, so a policy was a value a composer passed
//! in and never a value anybody could configure. This module is that step, and
//! it lives beside [`crate::boot`] for the same reason: a policy each surface
//! resolved for itself is a policy two surfaces can disagree about.
//!
//! Parity: upstream `resolveRetryPolicy` in
//! `packages/llm/llm/src/retry-policy.ts`, key for key and rule for rule. Two
//! deliberate differences, both recorded in `docs/parity.md`:
//!
//! - Upstream's policy is per provider route, read from that provider's own
//!   configuration block. tetanus has no per-provider block in the document
//!   yet, so one policy is resolved and installed on the route each session
//!   names. The executor is already route-scoped, so the block is all that is
//!   missing.
//! - Upstream caps every delay at `MAX_TIMER_DELAY_MS`, which is the largest
//!   value a JavaScript timer accepts. That is a limit of its runtime and not
//!   of the policy, and `tokio::time::sleep` has no such edge, so it is not
//!   ported. The bounds that mean something - positive, finite, and an initial
//!   delay no larger than the ceiling - are.

use serde_json::Value;
use tetanus_config::{Config, ConfigError, Document, Layer};
use tetanus_turn::llm::retry::{
    Backoff, RetryPolicy, DEFAULT_INITIAL_DELAY_MS, DEFAULT_JITTER_RATIO, DEFAULT_MAX_DELAY_MS,
    DEFAULT_MAX_RETRIES, DEFAULT_RETRYABLE_CODES,
};

use crate::boot::bad;

/// The keys a document sets a retry policy with. The nesting is upstream's,
/// flattened by the document reader: `llm: {retry: {backoff: {...}}}`.
pub mod key {
    pub const MODE: &str = "llm.retry.mode";
    pub const MAX_RETRIES: &str = "llm.retry.max_retries";
    pub const RETRYABLE_CODES: &str = "llm.retry.retryable_codes";
    pub const INITIAL_DELAY_MS: &str = "llm.retry.backoff.initial_delay_ms";
    pub const MAX_DELAY_MS: &str = "llm.retry.backoff.max_delay_ms";
    pub const JITTER_RATIO: &str = "llm.retry.backoff.jitter_ratio";
}

/// The two modes, as a document spells them.
pub const NORMAL: &str = "normal";
pub const ALWAYS: &str = "always";

/// The compiled defaults as a layer document, so every key above reports where
/// it came from even when no document mentions it.
pub fn defaults() -> Document {
    Document::from([
        (key::MODE.to_string(), Value::from(NORMAL)),
        (
            key::MAX_RETRIES.to_string(),
            Value::from(DEFAULT_MAX_RETRIES),
        ),
        (
            key::RETRYABLE_CODES.to_string(),
            Value::from(DEFAULT_RETRYABLE_CODES.to_vec()),
        ),
        (
            key::INITIAL_DELAY_MS.to_string(),
            Value::from(DEFAULT_INITIAL_DELAY_MS),
        ),
        (
            key::MAX_DELAY_MS.to_string(),
            Value::from(DEFAULT_MAX_DELAY_MS),
        ),
        (
            key::JITTER_RATIO.to_string(),
            Value::from(DEFAULT_JITTER_RATIO),
        ),
    ])
}

/// The policy `settings` describes, or the first key that describes nothing.
///
/// A value out of range is refused rather than clamped. The executor clamps
/// what it is given so a bad number cannot panic mid-turn, but a document is
/// somebody's stated intent, and silently running a different policy from the
/// one written down is how a retry storm goes unexplained.
pub fn policy(settings: &Config) -> Result<RetryPolicy, ConfigError> {
    let backoff = backoff(settings)?;
    match mode(settings)? {
        Mode::Always => {
            // Upstream refuses a key the chosen mode does not take, rather
            // than ignoring it: a `max_retries` beside `mode: always` means
            // its author expected a bound, and they are not getting one.
            for unusable in [key::MAX_RETRIES, key::RETRYABLE_CODES] {
                if configured(settings, unusable) {
                    return Err(ConfigError::BadValue {
                        key: unusable.to_string(),
                        expected: format!("unset: {ALWAYS} mode retries everything, for ever"),
                        found: found(settings, unusable),
                    });
                }
            }
            Ok(RetryPolicy::Always { backoff })
        }
        Mode::Normal => Ok(RetryPolicy::Normal {
            max_retries: max_retries(settings)?,
            retryable_codes: retryable_codes(settings)?,
            backoff,
        }),
    }
}

enum Mode {
    Normal,
    Always,
}

fn mode(settings: &Config) -> Result<Mode, ConfigError> {
    match settings.get(key::MODE) {
        None => Ok(Mode::Normal),
        Some(resolved) => match resolved.value.as_str() {
            Some(NORMAL) => Ok(Mode::Normal),
            Some(ALWAYS) => Ok(Mode::Always),
            _ => Err(bad(
                key::MODE,
                &format!("\"{NORMAL}\" or \"{ALWAYS}\""),
                &resolved.value,
            )),
        },
    }
}

fn max_retries(settings: &Config) -> Result<u32, ConfigError> {
    let Some(resolved) = settings.get(key::MAX_RETRIES) else {
        return Ok(DEFAULT_MAX_RETRIES);
    };
    // Zero is a policy, not a mistake: it says this route never retries, which
    // is the one thing an empty code list cannot say.
    match resolved.value.as_u64() {
        Some(retries) if retries <= u32::MAX as u64 => Ok(retries as u32),
        _ => Err(bad(
            key::MAX_RETRIES,
            "a whole number of retries, zero or more",
            &resolved.value,
        )),
    }
}

fn retryable_codes(settings: &Config) -> Result<Vec<String>, ConfigError> {
    let Some(resolved) = settings.get(key::RETRYABLE_CODES) else {
        return Ok(DEFAULT_RETRYABLE_CODES
            .iter()
            .map(|c| c.to_string())
            .collect());
    };
    let codes: Option<Vec<String>> = resolved.value.as_array().map(|codes| {
        codes
            .iter()
            .filter_map(|code| code.as_str())
            .filter(|code| !code.trim().is_empty())
            .map(str::to_string)
            .collect()
    });
    match codes {
        // The counts must agree, or something in the list was not a code: a
        // list quietly one element shorter than it was written is a route that
        // stops retrying a failure somebody listed.
        Some(codes)
            if !codes.is_empty()
                && codes.len() == resolved.value.as_array().map_or(0, Vec::len)
                && unique(&codes) =>
        {
            Ok(codes)
        }
        _ => Err(bad(
            key::RETRYABLE_CODES,
            "a list of distinct failure codes, not empty",
            &resolved.value,
        )),
    }
}

fn backoff(settings: &Config) -> Result<Backoff, ConfigError> {
    let initial_delay_ms = delay(settings, key::INITIAL_DELAY_MS, DEFAULT_INITIAL_DELAY_MS)?;
    let max_delay_ms = delay(settings, key::MAX_DELAY_MS, DEFAULT_MAX_DELAY_MS)?;
    if initial_delay_ms > max_delay_ms {
        return Err(bad(
            key::INITIAL_DELAY_MS,
            &format!("no longer than {}, the ceiling", key::MAX_DELAY_MS),
            &Value::from(initial_delay_ms),
        ));
    }
    Ok(Backoff {
        initial_delay_ms,
        max_delay_ms,
        jitter_ratio: ratio(settings)?,
    })
}

/// A wait in milliseconds. Zero is refused because a backoff that waits no
/// time is not a backoff: it is the request loop with no gap in it.
fn delay(settings: &Config, key: &str, fallback: f64) -> Result<f64, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(fallback);
    };
    match resolved.value.as_f64() {
        Some(ms) if ms.is_finite() && ms > 0.0 => Ok(ms),
        _ => Err(bad(
            key,
            "a wait in milliseconds, above zero",
            &resolved.value,
        )),
    }
}

fn ratio(settings: &Config) -> Result<f64, ConfigError> {
    let Some(resolved) = settings.get(key::JITTER_RATIO) else {
        return Ok(DEFAULT_JITTER_RATIO);
    };
    match resolved.value.as_f64() {
        Some(ratio) if (0.0..=1.0).contains(&ratio) => Ok(ratio),
        _ => Err(bad(
            key::JITTER_RATIO,
            "a spread between zero and one",
            &resolved.value,
        )),
    }
}

/// Whether a document, and not the defaults layer, set this key.
fn configured(settings: &Config, key: &str) -> bool {
    settings.get(key).is_some_and(|r| r.layer > Layer::Default)
}

fn found(settings: &Config, key: &str) -> String {
    settings
        .get(key)
        .map_or_else(|| "nothing".to_string(), |r| r.value.to_string())
}

fn unique(codes: &[String]) -> bool {
    codes
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == codes.len()
}
