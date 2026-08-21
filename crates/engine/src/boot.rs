//! The engine's own configuration, resolved out of the settings document.
//!
//! `crates/config` reads a document into layers and remembers where every
//! value came from. Turning those values into the settings the engine runs on
//! is this module, and it belongs to the engine for the reason
//! [`crate::catalog`] gives: a value each surface resolved for itself is a
//! value two surfaces can disagree about, and `config.dump` would then report
//! provenance for one of them.
//!
//! The keys are the ones [`crate::catalog::key`], [`crate::retry::key`] and
//! [`crate::tools::key`] name, so the list a `tetanus config` prints and the list a document may set
//! cannot drift apart. A key a document does not set keeps the compiled
//! default, and says so.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tetanus_config::{file, home, Config, ConfigError, Document, Layer};

use crate::catalog::key;
use crate::session::SessionBackend;
use crate::EngineConfig;

/// Read the settings document under the harness home, over the engine's own
/// defaults. `home` names a home directly; `None` is the discovered one.
pub fn settings(home: Option<&Path>) -> Result<Config, ConfigError> {
    document(&file::document_path(&home::home(home)))
}

/// [`settings`] for a document at a named path, which is what a test uses and
/// what a surface with its own `--settings` flag would.
///
/// An absent document is not a fault: a first run has none, and the answer is
/// then the defaults layer alone.
pub fn document(path: &Path) -> Result<Config, ConfigError> {
    let mut config = Config::default();
    config.load(Layer::Default, defaults());
    config.load(Layer::File, file::read(path)?);
    Ok(config)
}

/// The engine's compiled defaults as a layer document, so a value nobody
/// configured still reports where it came from rather than appearing from
/// nowhere.
pub fn defaults() -> Document {
    let base = EngineConfig::default();
    let mut document = Document::from([
        (
            key::SESSIONS_ROOT.to_string(),
            serde_json::json!(base.sessions_root.display().to_string()),
        ),
        (
            key::SESSIONS_BACKEND.to_string(),
            serde_json::json!(base.sessions_backend.name()),
        ),
        (
            key::PROVIDER.to_string(),
            serde_json::json!(base.default_provider),
        ),
        (
            key::MODEL.to_string(),
            serde_json::json!(base.default_model),
        ),
        (
            key::MAX_STEPS.to_string(),
            serde_json::json!(base.max_steps),
        ),
        (
            key::MAX_PARALLEL_TOOL_CALLS.to_string(),
            serde_json::json!(base.max_parallel_tool_calls.get()),
        ),
    ]);
    document.extend(crate::retry::defaults());
    document.extend(crate::tools::defaults());
    document
}

impl EngineConfig {
    /// The engine's settings as `config` resolves them, keeping the adapters
    /// and tools of [`EngineConfig::default`] for a caller to override.
    ///
    /// A value of the wrong type is refused rather than ignored. Ignoring it
    /// would run the engine on a setting the user did not write, and the
    /// document is the one place they said what they wanted.
    pub fn from_settings(settings: Config) -> Result<Self, ConfigError> {
        let base = Self::default();
        let sessions_root =
            text(&settings, key::SESSIONS_ROOT)?.map_or(base.sessions_root, PathBuf::from);
        Ok(Self {
            sessions_backend: backend(&settings, &sessions_root)?,
            sessions_root,
            default_provider: text(&settings, key::PROVIDER)?.unwrap_or(base.default_provider),
            default_model: text(&settings, key::MODEL)?.unwrap_or(base.default_model),
            max_steps: steps(&settings, key::MAX_STEPS)?.unwrap_or(base.max_steps),
            max_parallel_tool_calls: parallel(&settings, key::MAX_PARALLEL_TOOL_CALLS)?
                .unwrap_or(base.max_parallel_tool_calls),
            tool_order: crate::tools::order(&settings, &base.tools)?,
            retry: crate::retry::policy(&settings)?,
            provider_retry: crate::retry::provider_policies(&settings)?,
            providers: base.providers,
            tools: base.tools,
            resolved: Arc::new(settings),
        })
    }
}

/// The artifact this deployment keeps its journals in.
///
/// A name this build does not serve, and a database it cannot open, are both
/// refused here rather than at the first `session.create`: what a deployment
/// asked for is not available, and running on the other backend would put a
/// user's history somewhere they did not ask for it to go.
fn backend(settings: &Config, root: &Path) -> Result<SessionBackend, ConfigError> {
    let Some(name) = text(settings, key::SESSIONS_BACKEND)? else {
        return Ok(SessionBackend::Jsonl);
    };
    SessionBackend::named(&name, root).map_err(|message| ConfigError::BadValue {
        key: key::SESSIONS_BACKEND.to_string(),
        expected: "a session backend this build can open".to_string(),
        found: message,
    })
}

/// A key that holds a name or a path. Empty is not a name, and a document that
/// sets one has said something it cannot have meant.
fn text(settings: &Config, key: &str) -> Result<Option<String>, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(None);
    };
    match resolved.value.as_str() {
        Some(text) if !text.trim().is_empty() => Ok(Some(text.to_string())),
        _ => Err(bad(key, "a name", &resolved.value)),
    }
}

/// The step ceiling. Zero is not a ceiling a turn can run under, so it is a
/// mistake to report rather than a limit to honour.
fn steps(settings: &Config, key: &str) -> Result<Option<u32>, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(None);
    };
    match resolved.value.as_u64() {
        Some(steps) if (1..=u32::MAX as u64).contains(&steps) => Ok(Some(steps as u32)),
        _ => Err(bad(
            key,
            "a whole number of steps, one or more",
            &resolved.value,
        )),
    }
}

/// The parallel cap. A cap of one is serial dispatch, which is a thing to
/// ask for; a cap of none is a pool that can start nothing, which is not.
fn parallel(settings: &Config, key: &str) -> Result<Option<NonZeroUsize>, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(None);
    };
    match resolved
        .value
        .as_u64()
        .and_then(|calls| usize::try_from(calls).ok())
        .and_then(NonZeroUsize::new)
    {
        Some(calls) => Ok(Some(calls)),
        None => Err(bad(
            key,
            "a whole number of calls, one or more",
            &resolved.value,
        )),
    }
}

pub(crate) fn bad(key: &str, expected: &str, found: &serde_json::Value) -> ConfigError {
    ConfigError::BadValue {
        key: key.to_string(),
        expected: expected.to_string(),
        found: found.to_string(),
    }
}
