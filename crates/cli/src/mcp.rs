//! Starting the MCP servers a document declares, and stopping them again.
//!
//! `crates/mcp` could connect a server and put its tools in a registry from
//! the day it landed; nothing called it, so a deployment could declare a
//! server under `mcp.servers.*` and the binary would read the key, compose an
//! empty `mcp` source, and offer nothing. This is the call.
//!
//! **A server that will not start does not stop the harness.** Every server is
//! connected independently and the failures are reported beside the successes:
//! a laptop with one broken server in its document still gets a working agent
//! with the other servers' tools. That is `crates/mcp`'s rule; what this adds
//! is that the failure is *said*, on stderr, naming the server and its class.
//! A tool that is silently absent is a capability nobody took away.
//!
//! **Nothing this process starts outlives it.** [`Servers::shutdown`] runs the
//! close-input, wait, kill ladder over every supervisor before the surface
//! returns. `kill_on_drop` is the backstop for the paths that never get there,
//! a panic or a signal, but the ordinary exit goes through the ladder, so a
//! server gets to finish writing whatever it was writing.

use std::sync::Arc;

use tetanus_config::Config;
use tetanus_mcp::settings::{Connected, NotConnected};
use tetanus_turn::tools::{Tool, ToolRegistry};

use crate::{misconfigured, Policy, Reported};

/// The servers this run started, and what they contribute.
///
/// Held by the surface for as long as its tools might be called, because a
/// supervisor dropped is a server killed: the tools in `tools` hold `Arc`s to
/// these supervisors, and a registry outliving them would answer every call
/// with a dead connection.
pub struct Servers {
    connected: Vec<Connected>,
    /// The declared servers that are not serving, with the reason each gave.
    pub refused: Vec<NotConnected>,
    /// Every tool the connected servers advertise, ready for the assembly.
    pub tools: Vec<Arc<dyn Tool>>,
}

impl Servers {
    /// Nothing declared, nothing started. What a build with no `mcp.servers`
    /// composes, and what a surface uses when it has no runtime to connect on.
    pub fn none() -> Self {
        Self {
            connected: Vec::new(),
            refused: Vec::new(),
            tools: Vec::new(),
        }
    }

    /// Start every enabled server the document declares.
    ///
    /// The reconnect policy and the timeouts come from the same document, and
    /// an impossible one is refused here rather than at the first tool call -
    /// `crates/mcp` checks the policy when it reads it, and a report about a
    /// settings key belongs to the surface that read the file.
    pub async fn start(
        policy_out: &Policy,
        document: &std::path::Path,
        settings: &Arc<Config>,
    ) -> Result<Self, Reported> {
        let refuse = |err: tetanus_config::ConfigError| {
            misconfigured(
                policy_out,
                document,
                &tetanus_engine::convert::config_error(&err),
            )
        };
        let declared = tetanus_mcp::settings::servers(settings).map_err(refuse)?;
        if declared.iter().all(|server| !server.enabled) {
            return Ok(Self::none());
        }
        let reconnect = tetanus_mcp::settings::policy(settings).map_err(refuse)?;
        let timeouts = tetanus_mcp::settings::timeouts(settings).map_err(refuse)?;

        // `connect_all` registers rather than answering with a list, because
        // installing a bridged tool is the thing it exists to do. Draining a
        // throwaway registry is how the assembly takes the result without
        // `crates/mcp` growing a second entry point for one caller.
        let mut staging = ToolRegistry::new();
        let (connected, refused) =
            tetanus_mcp::settings::connect_all(&mut staging, &declared, reconnect, timeouts).await;
        let names: Vec<String> = staging.names().cloned().collect();
        let tools = names.iter().filter_map(|name| staging.get(name)).collect();

        Ok(Self {
            connected,
            refused,
            tools,
        })
    }

    /// The servers that are serving, by name, with what each contributed.
    pub fn serving(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.connected
            .iter()
            .map(|server| (server.name.as_str(), server.tools.as_slice()))
    }

    /// Say which declared servers are not serving, and why.
    ///
    /// On stderr, as a warning rather than an error: the run continues. It is
    /// written once at boot rather than when a tool is missing, because "the
    /// model never called the tool I configured" is a question whose answer
    /// has to be visible before the run, not deduced after it.
    pub fn report(&self, policy: &Policy) {
        let mut err = policy.stderr();
        // What *did* start is said too, one line per server, because it is the
        // answer to "why is this tool here" and to "did my configuration take
        // effect" - and a deployment that did not want the lines did not
        // declare a server. It costs nothing on the ordinary run, which
        // declares none.
        for (name, tools) in self.serving() {
            err.warn(&format!(
                "the MCP server {name:?} is serving {} {}",
                tools.len(),
                if tools.len() == 1 { "tool" } else { "tools" }
            ))
            .ok();
        }
        for server in &self.refused {
            err.warn(&format!(
                "the MCP server {:?} did not start [{}]: {}",
                server.name,
                server.fault.class(),
                server.fault
            ))
            .ok();
        }
    }

    /// Stop every server, over the ladder, before this surface returns.
    pub async fn shutdown(self) {
        for server in &self.connected {
            server.supervisor.shutdown().await;
        }
    }
}
