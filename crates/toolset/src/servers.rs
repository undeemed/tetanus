//! Starting the MCP servers a document declares, and stopping them again.
//!
//! `crates/mcp` could connect a server and put its tools in a registry from
//! the day it landed; nothing called it, so a deployment could declare a
//! server under `mcp.servers.*`, the binary would read the key, compose an
//! empty `mcp` source, and offer nothing. This is the call.
//!
//! It lives here rather than in `crates/cli` because the binary deliberately
//! depends on no tool crate - the assembly is what knows they exist - and
//! because the thing it produces is exactly what `Composition::mcp` takes.
//!
//! **A server that will not start does not stop the harness.** Every server is
//! connected independently and the failures are answered beside the successes:
//! a laptop with one broken server in its document still gets a working agent
//! with the other servers' tools. That is `crates/mcp`'s rule; what this adds
//! is that the failure is *reportable*: [`Servers::faults`] is what a surface
//! says out loud, because a tool that is silently absent is a capability
//! nobody took away.
//!
//! **Nothing this process starts outlives it.** [`Servers::shutdown`] runs the
//! close-input, wait, kill ladder over every supervisor before the surface
//! returns. `kill_on_drop` is the backstop for the paths that never get there,
//! a panic or a signal, but the ordinary exit goes through the ladder, so a
//! server gets to finish what it was writing.

use std::sync::Arc;

use tetanus_config::{Config, ConfigError};
use tetanus_mcp::settings::{Connected, NotConnected};
use tetanus_turn::tools::{Tool, ToolRegistry};

// `Composition::mcp` is what the answer is for; the link is in the doc above.

/// The servers this run started, and what they contribute.
///
/// Held by the caller for as long as its tools might be called: the tools in
/// [`Servers::tools`] hold `Arc`s to these supervisors, and dropping the last
/// one kills the server behind them.
pub struct Servers {
    connected: Vec<Connected>,
    /// The declared servers that are not serving, with the reason each gave.
    pub refused: Vec<NotConnected>,
    /// Every tool the connected servers advertise, ready for
    /// `Composition::mcp`.
    pub tools: Vec<Arc<dyn Tool>>,
}

impl Servers {
    /// Nothing declared, nothing started.
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
    /// an impossible one is refused here rather than at the first tool call.
    /// A document that declares nothing starts nothing and costs nothing,
    /// which is what keeps this on the boot path of every surface.
    pub async fn start(settings: &Arc<Config>) -> Result<Self, ConfigError> {
        let declared = tetanus_mcp::settings::servers(settings)?;
        if declared.iter().all(|server| !server.enabled) {
            return Ok(Self::none());
        }
        let reconnect = tetanus_mcp::settings::policy(settings)?;
        let timeouts = tetanus_mcp::settings::timeouts(settings)?;

        // `connect_all` registers rather than answering with a list, because
        // installing a bridged tool is the thing it exists to do. Draining a
        // throwaway registry is how the assembly takes the result without
        // `crates/mcp` growing a second entry point for one caller - the same
        // move `Source::registered` makes for the crates that only register.
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

    /// One line per declared server that is not serving: its name, the class
    /// of what went wrong, and the sentence.
    ///
    /// Formatted here rather than by the surface so every surface says the
    /// same thing, and reported at boot rather than when a tool turns out to
    /// be missing, because "the model never called the tool I configured" is a
    /// question whose answer has to be visible before the run.
    pub fn faults(&self) -> Vec<String> {
        self.refused
            .iter()
            .map(|server| {
                format!(
                    "the MCP server {:?} did not start [{}]: {}",
                    server.name,
                    server.fault.class(),
                    server.fault
                )
            })
            .collect()
    }

    /// Stop every server, over the ladder, before the caller returns.
    pub async fn shutdown(self) {
        for server in &self.connected {
            server.supervisor.shutdown().await;
        }
    }
}
