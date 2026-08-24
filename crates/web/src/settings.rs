//! Turning the settings document into the web tools a harness registers.
//!
//! ```yaml
//! web:
//!   tools: { fetch: true, search: true }
//!   fetch: { max_bytes: 5242880, max_chars: 100000, max_redirects: 5, timeout_ms: 30000 }
//!   search:
//!     provider: deepseek
//!     max_results: 5
//!     deepseek: { api_key: "...", base_url: "https://api.deepseek.com" }
//! ```
//!
//! **`web_search` is registered whether or not a provider is usable.** What
//! the model is offered follows what the deployment enabled, not what happens
//! to be reachable: a tool that vanished when a key expired would make the
//! model's behaviour change for a reason it cannot see or report. An
//! unconfigured search fails the call with `WEB_PROVIDER_UNAVAILABLE`, which
//! is a sentence somebody can act on. Upstream registers on the same rule -
//! "schema follows enablement, not availability".
//!
//! **A credential is read from the document, and the document withholds it
//! from `config.dump`.** `crates/config/src/secret.rs` already redacts a key
//! whose last segment says it is one, which is why this reads
//! `web.search.deepseek.api_key` rather than inventing a name of its own.

use std::sync::Arc;
use std::time::Duration;

use tetanus_config::{Config, ConfigError};
use tetanus_turn::tools::Tool;

use crate::fetch::FetchLimits;
use crate::live::LiveHttp;
use crate::provider::{DeepSeekSearch, DeepSeekSearchConfig};
use crate::search::WebRuntime;
use crate::tools::{WebFetchTool, WebSearchTool};

/// The keys this module reads.
pub mod key {
    pub const FETCH_ENABLED: &str = "web.tools.fetch";
    pub const SEARCH_ENABLED: &str = "web.tools.search";
    pub const MAX_BYTES: &str = "web.fetch.max_bytes";
    pub const MAX_CHARS: &str = "web.fetch.max_chars";
    pub const MAX_OUTPUT: &str = "web.fetch.max_output_chars";
    pub const MAX_REDIRECTS: &str = "web.fetch.max_redirects";
    pub const TIMEOUT: &str = "web.fetch.timeout_ms";
    pub const PROVIDER: &str = "web.search.provider";
    pub const MAX_RESULTS: &str = "web.search.max_results";
    pub const DEEPSEEK_KEY: &str = "web.search.deepseek.api_key";
    pub const DEEPSEEK_BASE: &str = "web.search.deepseek.base_url";
    pub const DEEPSEEK_MODEL: &str = "web.search.deepseek.model";
}

/// The environment variable a deployment usually supplies the key through.
/// The same one `crates/turn` reads for the provider itself, because it is the
/// same account.
pub const KEY_ENV: &str = "DEEPSEEK_API_KEY";

/// The limits a fetch runs under, as the document names them.
pub fn limits(settings: &Config) -> Result<FetchLimits, ConfigError> {
    let base = FetchLimits::default();
    Ok(FetchLimits {
        max_bytes: size(settings, key::MAX_BYTES)?.unwrap_or(base.max_bytes),
        max_chars: size(settings, key::MAX_CHARS)?.unwrap_or(base.max_chars),
        max_redirects: hops(settings, key::MAX_REDIRECTS)?.unwrap_or(base.max_redirects),
        timeout: millis(settings, key::TIMEOUT)?.unwrap_or(base.timeout),
    })
}

/// The search runtime the document describes, with the providers it configured
/// registered on it.
///
/// `key_from_env` is the credential the environment supplied, so a case can
/// pin the fallback without setting a process-wide variable.
pub fn runtime(settings: &Config, key_from_env: Option<&str>) -> Result<WebRuntime, ConfigError> {
    let deepseek = DeepSeekSearchConfig {
        api_key: text(settings, key::DEEPSEEK_KEY)?
            .or_else(|| key_from_env.map(str::to_string))
            .filter(|key| !key.trim().is_empty()),
        base_url: text(settings, key::DEEPSEEK_BASE)?
            .unwrap_or_else(|| crate::provider::DEFAULT_BASE_URL.to_string()),
        model: text(settings, key::DEEPSEEK_MODEL)?
            .unwrap_or_else(|| crate::provider::DEFAULT_MODEL.to_string()),
        ..DeepSeekSearchConfig::default()
    };
    Ok(WebRuntime::new()
        .with(Arc::new(DeepSeekSearch::new(LiveHttp::new(), deepseek)))
        .configure(text(settings, key::PROVIDER)?)
        .cap(size(settings, key::MAX_RESULTS)?))
}

/// The tools this document asks for, ready to register.
///
/// Both are off unless the document turns them on. A harness that reached the
/// network because nobody said otherwise would be a harness whose first run in
/// a sandbox is a surprise.
pub fn tools(
    settings: &Config,
    key_from_env: Option<&str>,
) -> Result<Vec<Arc<dyn Tool>>, ConfigError> {
    let mut registered: Vec<Arc<dyn Tool>> = Vec::new();
    if flag(settings, key::FETCH_ENABLED)?.unwrap_or(false) {
        let fetcher = WebFetchTool::new(Arc::new(LiveHttp::new())).limits(limits(settings)?);
        let fetcher = match size(settings, key::MAX_OUTPUT)? {
            Some(cap) => fetcher.max_output(cap),
            None => fetcher,
        };
        registered.push(Arc::new(fetcher));
    }
    if flag(settings, key::SEARCH_ENABLED)?.unwrap_or(false) {
        registered.push(Arc::new(WebSearchTool::new(Arc::new(runtime(
            settings,
            key_from_env,
        )?))));
    }
    Ok(registered)
}

fn flag(settings: &Config, key: &str) -> Result<Option<bool>, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(None);
    };
    resolved
        .value
        .as_bool()
        .map(Some)
        .ok_or_else(|| bad(key, "true or false", &resolved.value))
}

fn text(settings: &Config, key: &str) -> Result<Option<String>, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(None);
    };
    match resolved.value.as_str() {
        Some(text) if !text.trim().is_empty() => Ok(Some(text.trim().to_string())),
        _ => Err(bad(key, "text with something in it", &resolved.value)),
    }
}

fn size(settings: &Config, key: &str) -> Result<Option<usize>, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(None);
    };
    resolved
        .value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .map(Some)
        .ok_or_else(|| bad(key, "a whole number, one or more", &resolved.value))
}

/// A hop cap. Zero is a real answer here - follow no redirect - which is why
/// this is not [`size`].
fn hops(settings: &Config, key: &str) -> Result<Option<u8>, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(None);
    };
    resolved
        .value
        .as_u64()
        .and_then(|value| u8::try_from(value).ok())
        .map(Some)
        .ok_or_else(|| {
            bad(
                key,
                "a whole number of redirects, zero or more",
                &resolved.value,
            )
        })
}

fn millis(settings: &Config, key: &str) -> Result<Option<Duration>, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(None);
    };
    resolved
        .value
        .as_u64()
        .filter(|ms| *ms > 0)
        .map(|ms| Some(Duration::from_millis(ms)))
        .ok_or_else(|| bad(key, "a positive number of milliseconds", &resolved.value))
}

fn bad(key: &str, expected: &str, found: &serde_json::Value) -> ConfigError {
    ConfigError::BadValue {
        key: key.to_string(),
        expected: expected.to_string(),
        found: found.to_string(),
    }
}
