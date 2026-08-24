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
//! - Upstream has no general block: every route reads its own provider's
//!   configuration and nothing else. tetanus keeps a general `llm.retry`
//!   block beside the per-provider ones, because one policy is what most
//!   deployments want and repeating it per provider is how the copies drift.
//!   A provider's own block is still the whole policy for that route rather
//!   than a patch on the general one - see [`provider_policy`].
//! - Upstream caps every delay at `MAX_TIMER_DELAY_MS`, which is the largest
//!   value a JavaScript timer accepts. That is a limit of its runtime and not
//!   of the policy, and `tokio::time::sleep` has no such edge, so it is not
//!   ported. The bounds that mean something - positive, finite, and an initial
//!   delay no larger than the ceiling - are.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tetanus_config::schema::{Field, Kind, Schema};
use tetanus_config::{Config, ConfigError, Document, Layer};
use tetanus_turn::llm::retry::{
    Backoff, RetryPolicy, DEFAULT_INITIAL_DELAY_MS, DEFAULT_JITTER_RATIO, DEFAULT_MAX_DELAY_MS,
    DEFAULT_MAX_RETRIES, DEFAULT_RETRYABLE_CODES,
};

use crate::boot::bad;

/// The keys a document sets a retry policy with. The nesting is upstream's,
/// flattened by the document reader: `llm: {retry: {backoff: {...}}}`.
pub mod key {
    /// The general block, which every route runs unless its own says
    /// otherwise. The six constants below are this prefix, suffixed.
    pub const RETRY: &str = "llm.retry";
    /// Where the providers a document configures live.
    pub const PROVIDERS: &str = "llm.providers";
    /// The infix that marks a provider's key as part of its retry block.
    pub const RETRY_INFIX: &str = ".retry.";

    /// One provider's own block: `llm.providers.<provider>.retry`.
    pub fn provider_retry(provider: &str) -> String {
        format!("{PROVIDERS}.{provider}.retry")
    }

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

/// What the retry block claims about its own keys.
///
/// A provider's own block is not declared: the provider names are a
/// deployment's, not this crate's, so `llm.providers.<name>.retry.*` is checked
/// by `provider_policies` when it reads them. What the declaration buys here is
/// the general block - a document that writes `llm.retry: off` is refused
/// instead of quietly leaving every retry key at its default.
pub fn declare(schema: &mut Schema) {
    schema
        .declare(key::MODE, Field::new(Kind::Text))
        .declare(key::MAX_RETRIES, Field::new(Kind::Integer))
        .declare(key::RETRYABLE_CODES, Field::new(Kind::List))
        .declare(key::INITIAL_DELAY_MS, Field::new(Kind::Integer))
        .declare(key::MAX_DELAY_MS, Field::new(Kind::Integer))
        .declare(key::JITTER_RATIO, Field::new(Kind::Number));
}

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
    resolve(settings, &Keys::under(key::RETRY))
}

/// The policy `provider`'s own block describes, or `None` when the document
/// gives it no block.
///
/// A block is the whole policy for that route, resolved over the compiled
/// defaults rather than over the general block. Layering the two would make
/// `mode: always` inherit a `max_retries` its author never wrote, and the rule
/// that refuses an unusable key would then fire on a key nobody stated. So the
/// choice is per route and it is stated in one place: either this provider has
/// a block and that block says everything, or it has none and runs the general
/// policy.
pub fn provider_policy(
    settings: &Config,
    provider: &str,
) -> Result<Option<RetryPolicy>, ConfigError> {
    let keys = Keys::under(&key::provider_retry(provider));
    if !keys.any_configured(settings) {
        return Ok(None);
    }
    Ok(Some(resolve(settings, &keys)?))
}

/// Every provider the document gives a retry block, with the policy it
/// describes.
///
/// The names come from the document, because a policy may be written for a
/// provider this build has no adapter for: a document is read before the
/// engine is built, and refusing an unknown name here would make configuring a
/// provider you have not installed yet an error instead of a plan.
pub fn provider_policies(settings: &Config) -> Result<BTreeMap<String, RetryPolicy>, ConfigError> {
    let mut policies = BTreeMap::new();
    for provider in configured_providers(settings)? {
        let keys = Keys::under(&key::provider_retry(&provider));
        policies.insert(provider, resolve(settings, &keys)?);
    }
    Ok(policies)
}

/// The provider names a retry block is written under, in one settled order.
fn configured_providers(settings: &Config) -> Result<BTreeSet<String>, ConfigError> {
    let prefix = format!("{}.", key::PROVIDERS);
    let mut providers = BTreeSet::new();
    for (full, resolved) in settings.provenance() {
        let Some(under) = full.strip_prefix(&prefix) else {
            continue;
        };
        // From the right: the provider is what comes before its own block, and
        // a name may hold dots of its own.
        let Some((provider, _)) = under.rsplit_once(key::RETRY_INFIX) else {
            continue;
        };
        if provider.trim().is_empty() {
            return Err(bad(
                full,
                "a provider name before its block",
                &resolved.value,
            ));
        }
        providers.insert(provider.to_string());
    }
    Ok(providers)
}

fn resolve(settings: &Config, keys: &Keys) -> Result<RetryPolicy, ConfigError> {
    let backoff = backoff(settings, keys)?;
    match mode(settings, keys)? {
        Mode::Always => {
            // Upstream refuses a key the chosen mode does not take, rather
            // than ignoring it: a `max_retries` beside `mode: always` means
            // its author expected a bound, and they are not getting one.
            for unusable in [&keys.max_retries, &keys.retryable_codes] {
                if configured(settings, unusable) {
                    return Err(ConfigError::BadValue {
                        key: unusable.clone(),
                        expected: format!("unset: {ALWAYS} mode retries everything, for ever"),
                        found: found(settings, unusable),
                    });
                }
            }
            Ok(RetryPolicy::Always { backoff })
        }
        Mode::Normal => Ok(RetryPolicy::Normal {
            max_retries: max_retries(settings, keys)?,
            retryable_codes: retryable_codes(settings, keys)?,
            backoff,
        }),
    }
}

enum Mode {
    Normal,
    Always,
}

/// The six keys of one block, general or per provider.
///
/// Built from a prefix rather than written out twice, so a rule can only be
/// enforced on both blocks or on neither.
struct Keys {
    mode: String,
    max_retries: String,
    retryable_codes: String,
    initial_delay_ms: String,
    max_delay_ms: String,
    jitter_ratio: String,
}

impl Keys {
    fn under(prefix: &str) -> Self {
        Self {
            mode: format!("{prefix}.mode"),
            max_retries: format!("{prefix}.max_retries"),
            retryable_codes: format!("{prefix}.retryable_codes"),
            initial_delay_ms: format!("{prefix}.backoff.initial_delay_ms"),
            max_delay_ms: format!("{prefix}.backoff.max_delay_ms"),
            jitter_ratio: format!("{prefix}.backoff.jitter_ratio"),
        }
    }

    /// Whether a document set any key of this block. The defaults layer sets
    /// the general block's keys and no provider's, which is what makes an
    /// absent provider block distinguishable from one that repeats a default.
    fn any_configured(&self, settings: &Config) -> bool {
        [
            &self.mode,
            &self.max_retries,
            &self.retryable_codes,
            &self.initial_delay_ms,
            &self.max_delay_ms,
            &self.jitter_ratio,
        ]
        .into_iter()
        .any(|key| configured(settings, key))
    }
}

fn mode(settings: &Config, keys: &Keys) -> Result<Mode, ConfigError> {
    match settings.get(&keys.mode) {
        None => Ok(Mode::Normal),
        Some(resolved) => match resolved.value.as_str() {
            Some(NORMAL) => Ok(Mode::Normal),
            Some(ALWAYS) => Ok(Mode::Always),
            _ => Err(bad(
                &keys.mode,
                &format!("\"{NORMAL}\" or \"{ALWAYS}\""),
                &resolved.value,
            )),
        },
    }
}

fn max_retries(settings: &Config, keys: &Keys) -> Result<u32, ConfigError> {
    let Some(resolved) = settings.get(&keys.max_retries) else {
        return Ok(DEFAULT_MAX_RETRIES);
    };
    // Zero is a policy, not a mistake: it says this route never retries, which
    // is the one thing an empty code list cannot say.
    match resolved.value.as_u64() {
        Some(retries) if retries <= u32::MAX as u64 => Ok(retries as u32),
        _ => Err(bad(
            &keys.max_retries,
            "a whole number of retries, zero or more",
            &resolved.value,
        )),
    }
}

fn retryable_codes(settings: &Config, keys: &Keys) -> Result<Vec<String>, ConfigError> {
    let Some(resolved) = settings.get(&keys.retryable_codes) else {
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
            &keys.retryable_codes,
            "a list of distinct failure codes, not empty",
            &resolved.value,
        )),
    }
}

fn backoff(settings: &Config, keys: &Keys) -> Result<Backoff, ConfigError> {
    let initial_delay_ms = delay(settings, &keys.initial_delay_ms, DEFAULT_INITIAL_DELAY_MS)?;
    let max_delay_ms = delay(settings, &keys.max_delay_ms, DEFAULT_MAX_DELAY_MS)?;
    if initial_delay_ms > max_delay_ms {
        return Err(bad(
            &keys.initial_delay_ms,
            &format!("no longer than {}, the ceiling", keys.max_delay_ms),
            &Value::from(initial_delay_ms),
        ));
    }
    Ok(Backoff {
        initial_delay_ms,
        max_delay_ms,
        jitter_ratio: ratio(settings, keys)?,
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

fn ratio(settings: &Config, keys: &Keys) -> Result<f64, ConfigError> {
    let Some(resolved) = settings.get(&keys.jitter_ratio) else {
        return Ok(DEFAULT_JITTER_RATIO);
    };
    match resolved.value.as_f64() {
        Some(ratio) if (0.0..=1.0).contains(&ratio) => Ok(ratio),
        _ => Err(bad(
            &keys.jitter_ratio,
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
