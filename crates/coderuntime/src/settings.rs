//! Turning the settings document into a code runtime a deployment can use.
//!
//! ```yaml
//! code:
//!   enabled: true
//!   budget: { fuel: 5000000, wall_ms: 30000, max_output_bytes: 1048576 }
//!   tools: [read_file, list_files]     # what a program may call
//!   remote:
//!     enabled: false
//!     api_key: "..."
//!     cwd: /home/user/workspace
//! ```
//!
//! **Off unless the document says otherwise**, for the reason
//! `crates/web/src/settings.rs` gives about the network: a capability a
//! deployment did not ask for should not appear because a crate was compiled
//! in.
//!
//! **The tools a program may call are named here, one by one.** Not "all of
//! them", because the registry grows whenever a plugin registers something and
//! a list that grows by itself is a list nobody decided.
//!
//! This is completeness rather than upstream parity: upstream's runtime is a
//! Cordis plugin, so its configuration is the plugin's and a deployment that
//! loads it has already chosen. A tetanus registry is compiled in, so the
//! document is where that choice lives.

use std::sync::Arc;
use std::time::Duration;

use tetanus_config::{Config, ConfigError};
use tetanus_turn::tools::{Tool, ToolRegistry};

use crate::local::{Budget, LocalRuntime};
use crate::remote::{RemoteRuntime, Sandbox, SandboxConfig};
use crate::tool::{tools_namespace, CodeTool};
use crate::types::CodeRuntime;

/// The keys this module reads.
pub mod key {
    pub const ENABLED: &str = "code.enabled";
    pub const FUEL: &str = "code.budget.fuel";
    pub const WALL: &str = "code.budget.wall_ms";
    pub const MAX_OUTPUT: &str = "code.budget.max_output_bytes";
    pub const REAP_GRACE: &str = "code.budget.reap_grace_ms";
    pub const TOOLS: &str = "code.tools";
    pub const REMOTE_ENABLED: &str = "code.remote.enabled";
    pub const REMOTE_KEY: &str = "code.remote.api_key";
    pub const REMOTE_CWD: &str = "code.remote.cwd";
    pub const REMOTE_WALL: &str = "code.remote.wall_ms";
}

/// The namespace a program's tools are offered under.
pub const TOOLS_GLOBAL: &str = "tools";

/// The budgets a program runs under, as the document names them.
pub fn budget(settings: &Config) -> Result<Budget, ConfigError> {
    let base = Budget::default();
    Ok(Budget {
        fuel: count(settings, key::FUEL)?.unwrap_or(base.fuel),
        wall: millis(settings, key::WALL)?.unwrap_or(base.wall),
        max_output_bytes: size(settings, key::MAX_OUTPUT)?.unwrap_or(base.max_output_bytes),
        reap_grace: millis(settings, key::REAP_GRACE)?.unwrap_or(base.reap_grace),
    })
}

/// How a remote runtime would be set up, if one is asked for.
pub fn remote_config(
    settings: &Config,
    key_from_env: Option<&str>,
) -> Result<SandboxConfig, ConfigError> {
    let base = SandboxConfig::default();
    Ok(SandboxConfig {
        api_key: text(settings, key::REMOTE_KEY)?.or_else(|| key_from_env.map(str::to_string)),
        cwd: text(settings, key::REMOTE_CWD)?.unwrap_or(base.cwd),
        wall: millis(settings, key::REMOTE_WALL)?.unwrap_or(base.wall),
        ..base
    })
}

/// Whether the document asks for a remote runtime rather than a local one.
pub fn wants_remote(settings: &Config) -> Result<bool, ConfigError> {
    Ok(flag(settings, key::REMOTE_ENABLED)?.unwrap_or(false))
}

/// The `run_code` tool this document asks for, if it asks for one.
///
/// `registry` is the tools already registered, which is what a program may be
/// offered; `remote` is the provider a deployment wired up, needed only when
/// the document turns the remote backend on.
pub fn tool(
    settings: &Config,
    registry: Arc<ToolRegistry>,
    remote: Option<Arc<dyn Sandbox>>,
    key_from_env: Option<&str>,
) -> Result<Option<Arc<dyn Tool>>, ConfigError> {
    if !flag(settings, key::ENABLED)?.unwrap_or(false) {
        return Ok(None);
    }

    let runtime: Arc<dyn CodeRuntime> = match (wants_remote(settings)?, remote) {
        (true, Some(provider)) => Arc::new(RemoteRuntime::new(
            provider,
            remote_config(settings, key_from_env)?,
        )),
        // A document that asked for the remote backend and a composer that
        // wired no provider is a mistake worth naming, not a quiet fallback
        // to running the program on this machine.
        (true, None) => {
            return Err(ConfigError::BadValue {
                key: key::REMOTE_ENABLED.to_string(),
                expected: "a remote provider wired into the composition".to_string(),
                found: "true, with no provider".to_string(),
            })
        }
        (false, _) => Arc::new(LocalRuntime::new(budget(settings)?)),
    };

    let offered = names(settings, key::TOOLS)?.unwrap_or_default();
    let mut code = CodeTool::new(runtime);
    if !offered.is_empty() {
        let namespace = tools_namespace(TOOLS_GLOBAL, registry, &offered).map_err(|why| {
            ConfigError::BadValue {
                key: key::TOOLS.to_string(),
                expected: "tools a program may call".to_string(),
                found: why.to_string(),
            }
        })?;
        code = code.binding(namespace);
    }
    Ok(Some(Arc::new(code)))
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

fn names(settings: &Config, key: &str) -> Result<Option<Vec<String>>, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(None);
    };
    let listed = resolved
        .value
        .as_array()
        .ok_or_else(|| bad(key, "a list of tool names", &resolved.value))?;
    listed
        .iter()
        .map(|name| match name.as_str() {
            Some(name) if !name.trim().is_empty() => Ok(name.trim().to_string()),
            _ => Err(bad(key, "a list of tool names", &resolved.value)),
        })
        .collect::<Result<Vec<String>, ConfigError>>()
        .map(Some)
}

fn count(settings: &Config, key: &str) -> Result<Option<u64>, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(None);
    };
    resolved
        .value
        .as_u64()
        .filter(|value| *value > 0)
        .map(Some)
        .ok_or_else(|| bad(key, "a whole number, one or more", &resolved.value))
}

fn size(settings: &Config, key: &str) -> Result<Option<usize>, ConfigError> {
    Ok(count(settings, key)?.and_then(|value| usize::try_from(value).ok()))
}

fn millis(settings: &Config, key: &str) -> Result<Option<Duration>, ConfigError> {
    Ok(count(settings, key)?.map(Duration::from_millis))
}

fn bad(key: &str, expected: &str, found: &serde_json::Value) -> ConfigError {
    ConfigError::BadValue {
        key: key.to_string(),
        expected: expected.to_string(),
        found: found.to_string(),
    }
}
