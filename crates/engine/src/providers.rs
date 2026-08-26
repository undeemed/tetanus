//! The model providers a settings document declares, and the registry every
//! surface routes through.
//!
//! `llm.providers.<name>.*` is a namespace the contract and [`crate::retry`]
//! already reserved: a retry block is written under it, and until now nothing
//! read anything else there. A deployment could therefore configure how a
//! provider retries and not which provider it was. This module reads the rest
//! of the block - where the route lives, which variable holds its key, what it
//! advertises - and turns it into an adapter.
//!
//! Every such route is realized by [`OpenAiCompatAdapter`], which is the
//! DeepSeek transport under a name a document wrote. That is the whole scope
//! of this module: any endpoint speaking `POST {base_url}/chat/completions`
//! with an SSE stream. A native API of another shape - Anthropic Messages, for
//! one - arrives as a second [`LlmAdapter`] implementation listed by the same
//! [`ProviderSet`], and nothing here has to change when it does.
//!
//! Unrecognized keys under the namespace are ignored rather than refused, for
//! the reason `crates/config/src/schema.rs` gives: the schema narrows what can
//! go wrong and is not a register a plugin must join. The credential store
//! writes `llm.providers.<name>.api_key` into this same namespace, and a
//! reader that refused what it did not recognize would make storing a
//! credential a boot failure.

use std::collections::BTreeSet;
use std::sync::Arc;

use tetanus_config::{Config, ConfigError};
use tetanus_turn::llm::deepseek::{self, DeepSeekAdapter, DeepSeekConfig};
use tetanus_turn::llm::mock::MockAdapter;
use tetanus_turn::llm::openai_compat::OpenAiCompatAdapter;
use tetanus_turn::llm::LlmAdapter;

use crate::agent::Providers;
use crate::boot::bad;

/// The keys one provider block is written with, under
/// [`crate::retry::key::PROVIDERS`].
pub mod key {
    /// Where the route lives: `https://openrouter.ai/api/v1`, with
    /// `/chat/completions` appended per request.
    pub const BASE_URL: &str = "base_url";
    /// The name of the environment variable holding the credential. A
    /// document never carries the key itself.
    pub const API_KEY_ENV: &str = "api_key_env";
    /// The advisory catalog this route advertises. An unlisted id still
    /// passes through, as it does for every adapter.
    pub const MODELS: &str = "models";
    pub const MAX_TOKENS: &str = "max_tokens";
    pub const STREAM_IDLE_TIMEOUT_MS: &str = "stream_idle_timeout_ms";
    pub const REQUEST_DEADLINE_MS: &str = "request_deadline_ms";

    /// Every leaf that makes a block exist. A key under the namespace that is
    /// none of these - `retry.*`, or the `api_key` the credential store
    /// writes - neither declares a provider nor refuses one.
    pub const LEAVES: [&str; 6] = [
        BASE_URL,
        API_KEY_ENV,
        MODELS,
        MAX_TOKENS,
        STREAM_IDLE_TIMEOUT_MS,
        REQUEST_DEADLINE_MS,
    ];
}

/// The routes this build serves without being told to, which a document may
/// not take over.
///
/// A document that wrote `llm.providers.mock.base_url` would otherwise
/// register a second adapter under a name already answered by the built-in
/// one, and which of the two a session got would be decided by list order. The
/// two spellings of the built-in route are both reserved for the same reason:
/// the CLI says `deepseek` and the adapter says `deepseek-official`, and a
/// document may take neither.
pub const RESERVED: [&str; 3] = ["mock", "deepseek", deepseek::PROVIDER];

/// Every provider the document declares, with the configuration it wrote, in
/// one settled order.
///
/// A name is refused when it collides with a built-in route or is not a name
/// at all. Everything else that can be wrong inside a block is refused naming
/// the key, the same way [`crate::retry`] refuses a bad policy: a document
/// that meant to configure a route and got a character wrong must not run as
/// though it had said nothing.
pub fn custom_providers(settings: &Config) -> Result<Vec<(String, DeepSeekConfig)>, ConfigError> {
    let mut configured = Vec::new();
    for name in declared(settings)? {
        let config = block(settings, &name)?;
        configured.push((name, config));
    }
    Ok(configured)
}

/// The provider names a block is written under, in one settled order.
fn declared(settings: &Config) -> Result<BTreeSet<String>, ConfigError> {
    let prefix = format!("{}.", crate::retry::key::PROVIDERS);
    let mut names = BTreeSet::new();
    for (full, resolved) in settings.provenance() {
        let Some(under) = full.strip_prefix(&prefix) else {
            continue;
        };
        // From the right, as the retry reader does it: a provider name may
        // hold dots of its own, so what identifies the block is its leaf.
        let Some((name, leaf)) = under.rsplit_once('.') else {
            continue;
        };
        if !key::LEAVES.contains(&leaf) {
            continue;
        }
        if name.trim().is_empty() {
            return Err(bad(
                full,
                "a provider name before its block",
                &resolved.value,
            ));
        }
        if RESERVED.contains(&name) {
            return Err(bad(
                full,
                &format!("a name this build does not already serve, not one of {RESERVED:?}"),
                &resolved.value,
            ));
        }
        names.insert(name.to_string());
    }
    Ok(names)
}

/// One block, resolved over the transport's own defaults.
fn block(settings: &Config, name: &str) -> Result<DeepSeekConfig, ConfigError> {
    let base = DeepSeekConfig::default();
    Ok(DeepSeekConfig {
        base_url: required(settings, name, key::BASE_URL)?,
        api_key_env: required(settings, name, key::API_KEY_ENV)?,
        models: models(settings, name)?,
        max_tokens: max_tokens(settings, name)?,
        // Zero is the adapter's own "unset", read by `DeepSeekConfig::idle_window`
        // and `::deadline`, so an unwritten key and a zero settle the same way.
        stream_idle_timeout_ms: millis(settings, name, key::STREAM_IDLE_TIMEOUT_MS)?
            .unwrap_or(base.stream_idle_timeout_ms),
        request_deadline_ms: millis(settings, name, key::REQUEST_DEADLINE_MS)?
            .unwrap_or(base.request_deadline_ms),
    })
}

/// `llm.providers.<name>.<leaf>`, written out once so a reader and a message
/// cannot name different keys.
fn full(name: &str, leaf: &str) -> String {
    format!("{}.{name}.{leaf}", crate::retry::key::PROVIDERS)
}

/// A key the block cannot be built without. There is no default for either:
/// a route with no address is not a route, and a credential the harness
/// resolves from nowhere fails on the first turn instead of at boot.
fn required(settings: &Config, name: &str, leaf: &str) -> Result<String, ConfigError> {
    let key = full(name, leaf);
    let Some(resolved) = settings.get(&key) else {
        return Err(bad(
            &key,
            "a value: this key is required",
            &serde_json::Value::Null,
        ));
    };
    match resolved.value.as_str() {
        Some(text) if !text.trim().is_empty() => Ok(text.to_string()),
        _ => Err(bad(&key, "a non-empty string", &resolved.value)),
    }
}

/// The advisory catalog. Absent is an empty list, which the panel and
/// `tetanus models` both already render as "names no models".
fn models(settings: &Config, name: &str) -> Result<Vec<String>, ConfigError> {
    let key = full(name, key::MODELS);
    let Some(resolved) = settings.get(&key) else {
        return Ok(Vec::new());
    };
    let listed: Option<Vec<String>> = resolved.value.as_array().map(|models| {
        models
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .map(str::to_string)
            .collect()
    });
    match listed {
        // The counts must agree, as they must for the retryable codes and for
        // the same reason: a list quietly one element shorter than it was
        // written advertises a catalog nobody wrote.
        Some(models) if models.len() == resolved.value.as_array().map_or(0, Vec::len) => Ok(models),
        _ => Err(bad(&key, "a list of model ids", &resolved.value)),
    }
}

/// The adapter-configured output cap. An explicit request value still wins
/// over it, which is the wrapped adapter's rule and not this reader's.
fn max_tokens(settings: &Config, name: &str) -> Result<Option<u32>, ConfigError> {
    let key = full(name, key::MAX_TOKENS);
    let Some(resolved) = settings.get(&key) else {
        return Ok(None);
    };
    match resolved.value.as_u64() {
        Some(tokens) if tokens > 0 && tokens <= u64::from(u32::MAX) => Ok(Some(tokens as u32)),
        _ => Err(bad(&key, "a token cap above zero", &resolved.value)),
    }
}

/// One of the two bounds, in milliseconds. Zero is kept rather than refused:
/// the adapter reads a zero as its own default, and refusing it here would
/// make "leave this on the default" spell differently from "unset".
fn millis(settings: &Config, name: &str, leaf: &str) -> Result<Option<u64>, ConfigError> {
    let key = full(name, leaf);
    let Some(resolved) = settings.get(&key) else {
        return Ok(None);
    };
    match resolved.value.as_u64() {
        Some(ms) => Ok(Some(ms)),
        None => Err(bad(&key, "a wait in milliseconds", &resolved.value)),
    }
}

/// Every adapter a build serves: the two built-in routes, then each route the
/// document declared.
///
/// The order is the order a catalog lists them in, and it puts the two that
/// need no configuration first so a picker opens on something usable. A
/// document's own routes follow in the settled order [`custom_providers`]
/// returns them in.
pub struct ProviderSet(Vec<Arc<dyn LlmAdapter>>);

impl ProviderSet {
    /// The registry a document describes, over the built-in routes.
    pub fn from_settings(settings: &Config) -> Result<Self, ConfigError> {
        Ok(Self::composed(custom_providers(settings)?))
    }

    /// The same registry from blocks already resolved, which is what a caller
    /// holding a `Vec` from [`custom_providers`] has.
    pub fn composed(custom: Vec<(String, DeepSeekConfig)>) -> Self {
        let mut adapters: Vec<Arc<dyn LlmAdapter>> = vec![
            Arc::new(MockAdapter::new()),
            Arc::new(DeepSeekAdapter::with_http(DeepSeekConfig::default())),
        ];
        for (name, config) in custom {
            adapters.push(Arc::new(OpenAiCompatAdapter::with_http(name, config)));
        }
        Self(adapters)
    }
}

impl Providers for ProviderSet {
    fn all(&self) -> Vec<Arc<dyn LlmAdapter>> {
        self.0.clone()
    }
}
