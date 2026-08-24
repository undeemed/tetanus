//! The servers a deployment runs, read out of the settings document.
//!
//! Without this an MCP server is something a composer writes Rust to start,
//! which means nobody turns one on. The document names them:
//!
//! ```yaml
//! mcp:
//!   reconnect: { initial_delay_ms: 500, max_delay_ms: 30000, max_attempts: 10 }
//!   request_timeout_ms: 30000
//!   servers:
//!     files:
//!       command: mcp-server-filesystem
//!       args: ["/srv/project"]
//!       env: { RUST_LOG: warn }
//! ```
//!
//! **A server that will not start does not stop the harness.** Every server is
//! connected independently and the failures are reported beside the successes:
//! a laptop with one broken server in its document still gets a working agent
//! with the other servers' tools, and the one that failed is named. The
//! alternative - refusing to boot - makes one bad line in a document a harness
//! nobody can use, and the tools are an addition, not a prerequisite.
//!
//! **An environment is what the document lists.** The reasoning is
//! `crates/turn/src/process.rs`'s, and it is why `env` is a map rather than a
//! flag: a server started with this process's whole environment is a program a
//! settings document chose, holding every credential the harness holds.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tetanus_config::{Config, ConfigError};
use tetanus_turn::tools::ToolRegistry;

use crate::client::{ClientInfo, Timeouts};
use crate::fault::McpFault;
use crate::stdio::ServerCommand;
use crate::supervisor::{Launcher, ReconnectPolicy, Supervisor};

/// The keys this module reads.
pub mod key {
    pub const SERVERS: &str = "mcp.servers.";
    pub const RECONNECT_ENABLED: &str = "mcp.reconnect.enabled";
    pub const INITIAL_DELAY: &str = "mcp.reconnect.initial_delay_ms";
    pub const MAX_DELAY: &str = "mcp.reconnect.max_delay_ms";
    pub const MAX_ATTEMPTS: &str = "mcp.reconnect.max_attempts";
    pub const HANDSHAKE_TIMEOUT: &str = "mcp.handshake_timeout_ms";
    pub const REQUEST_TIMEOUT: &str = "mcp.request_timeout_ms";
}

/// One server a document declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSettings {
    pub name: String,
    pub command: ServerCommand,
    /// Whether to start it at all. A server switched off stays in the document
    /// with its configuration intact, which is what a user wants when they are
    /// bisecting a problem.
    pub enabled: bool,
}

/// What one server contributed, or why it contributed nothing.
pub struct Connected {
    pub name: String,
    /// The public names its tools were registered under.
    pub tools: Vec<String>,
    pub supervisor: Arc<Supervisor>,
}

/// A server named in the document that is not serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotConnected {
    pub name: String,
    pub fault: McpFault,
}

/// Every server the document declares, by name.
pub fn servers(settings: &Config) -> Result<Vec<ServerSettings>, ConfigError> {
    let mut found: BTreeMap<String, BTreeMap<String, serde_json::Value>> = BTreeMap::new();
    for (key, resolved) in settings.provenance() {
        let Some(rest) = key.strip_prefix(key::SERVERS) else {
            continue;
        };
        let Some((name, inner)) = rest.split_once('.') else {
            continue;
        };
        if name.is_empty() || inner.is_empty() {
            continue;
        }
        found
            .entry(name.to_string())
            .or_default()
            .insert(inner.to_string(), resolved.value.clone());
    }

    found
        .into_iter()
        .map(|(name, keys)| declared(&name, &keys))
        .collect()
}

/// The reconnect policy the document names, over the defaults.
pub fn policy(settings: &Config) -> Result<ReconnectPolicy, ConfigError> {
    let base = ReconnectPolicy::default();
    let resolved = ReconnectPolicy {
        enabled: flag(settings, key::RECONNECT_ENABLED)?.unwrap_or(base.enabled),
        initial_delay: millis(settings, key::INITIAL_DELAY)?.unwrap_or(base.initial_delay),
        max_delay: millis(settings, key::MAX_DELAY)?.unwrap_or(base.max_delay),
        max_attempts: count(settings, key::MAX_ATTEMPTS)?.unwrap_or(base.max_attempts),
    };
    // The policy's own rule, so a document and a composer are refused for the
    // same reasons and read the same message.
    resolved.resolve().map_err(|refused| ConfigError::BadValue {
        key: "mcp.reconnect".to_string(),
        expected: "a policy this harness can run".to_string(),
        found: refused.to_string(),
    })
}

/// The budgets the document names, over the defaults.
pub fn timeouts(settings: &Config) -> Result<Timeouts, ConfigError> {
    let base = Timeouts::default();
    Ok(Timeouts {
        handshake: millis(settings, key::HANDSHAKE_TIMEOUT)?.unwrap_or(base.handshake),
        request: millis(settings, key::REQUEST_TIMEOUT)?.unwrap_or(base.request),
    })
}

/// Start every enabled server the document declares, and register what they
/// advertise.
///
/// Answers both halves: the servers that are serving, and the ones that are
/// not with the reason. A caller that wants to fail on the second half may;
/// the harness does not.
pub async fn connect_all(
    registry: &mut ToolRegistry,
    declared: &[ServerSettings],
    policy: ReconnectPolicy,
    timeouts: Timeouts,
) -> (Vec<Connected>, Vec<NotConnected>) {
    let mut connected = Vec::new();
    let mut refused = Vec::new();
    for server in declared.iter().filter(|server| server.enabled) {
        let launcher = Arc::new(server.command.clone()) as Arc<dyn Launcher>;
        match Supervisor::start(
            server.name.clone(),
            launcher,
            policy,
            timeouts,
            ClientInfo::default(),
        )
        .await
        {
            Ok((supervisor, tools)) => {
                let names = crate::tools::install(registry, &supervisor, &tools);
                connected.push(Connected {
                    name: server.name.clone(),
                    tools: names,
                    supervisor,
                });
            }
            Err(fault) => {
                tracing::warn!(server = server.name, %fault, "an MCP server did not start");
                refused.push(NotConnected {
                    name: server.name.clone(),
                    fault,
                });
            }
        }
    }
    (connected, refused)
}

/// One server's keys, read into a command.
fn declared(
    name: &str,
    keys: &BTreeMap<String, serde_json::Value>,
) -> Result<ServerSettings, ConfigError> {
    let program = keys
        .get("command")
        .and_then(serde_json::Value::as_str)
        .filter(|command| !command.trim().is_empty())
        .ok_or_else(|| ConfigError::BadValue {
            key: format!("{}{name}.command", key::SERVERS),
            expected: "the program that speaks MCP on its standard input".to_string(),
            found: keys
                .get("command")
                .map_or_else(|| "nothing".to_string(), ToString::to_string),
        })?;

    let mut command = ServerCommand::new(program.trim());
    if let Some(args) = keys.get("args") {
        let listed = args.as_array().ok_or_else(|| ConfigError::BadValue {
            key: format!("{}{name}.args", key::SERVERS),
            expected: "a list of arguments".to_string(),
            found: args.to_string(),
        })?;
        for arg in listed {
            let arg = arg.as_str().ok_or_else(|| ConfigError::BadValue {
                key: format!("{}{name}.args", key::SERVERS),
                expected: "a list of arguments, each of them text".to_string(),
                found: args.to_string(),
            })?;
            command = command.arg(arg);
        }
    }
    // `env` is a section, so its keys arrive flattened: `env.RUST_LOG`.
    for (inner, value) in keys {
        if let Some(variable) = inner.strip_prefix("env.") {
            let value = value.as_str().ok_or_else(|| ConfigError::BadValue {
                key: format!("{}{name}.{inner}", key::SERVERS),
                expected: "text: an environment variable is a string".to_string(),
                found: value.to_string(),
            })?;
            command = command.env(variable, value);
        }
    }
    if let Some(cwd) = keys.get("cwd").and_then(serde_json::Value::as_str) {
        command = command.cwd(cwd);
    }

    Ok(ServerSettings {
        name: name.to_string(),
        command,
        enabled: keys
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
    })
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

fn count(settings: &Config, key: &str) -> Result<Option<u32>, ConfigError> {
    let Some(resolved) = settings.get(key) else {
        return Ok(None);
    };
    resolved
        .value
        .as_u64()
        .and_then(|count| u32::try_from(count).ok())
        .filter(|count| *count > 0)
        .map(Some)
        .ok_or_else(|| {
            bad(
                key,
                "a whole number of attempts, one or more",
                &resolved.value,
            )
        })
}

fn bad(key: &str, expected: &str, found: &serde_json::Value) -> ConfigError {
    ConfigError::BadValue {
        key: key.to_string(),
        expected: expected.to_string(),
        found: found.to_string(),
    }
}
