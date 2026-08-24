//! What went wrong with a server that is not this process's.
//!
//! Every variant carries a [class](McpFault::class): one stable lowercase word
//! that says which kind of thing failed. The class is what a failed tool result
//! leads with, so an operator reading a journal can tell "the server said no"
//! from "the server never answered" from "the server is not there any more"
//! without reading prose that a server author wrote.

use std::time::Duration;

/// Every class [`McpFault::class`] can answer, in one place, so a reader of a
/// journal has a list to check a value against.
pub mod class {
    /// The handshake did not complete, so no tool was ever discovered.
    pub const HANDSHAKE: &str = "handshake";
    /// The server did not answer inside the budget for the call.
    pub const TIMEOUT: &str = "timeout";
    /// The pipe went away: the server exited, or its output ended.
    pub const TRANSPORT: &str = "transport";
    /// Something arrived that is not a message this protocol has.
    pub const PROTOCOL: &str = "protocol";
    /// The server answered, with a JSON-RPC error.
    pub const SERVER: &str = "server";
    /// The tool ran and reported failure (`isError`).
    pub const TOOL: &str = "tool";
    /// There is no live connection to ask: it is down, or gave up reconnecting.
    pub const UNAVAILABLE: &str = "unavailable";
    /// The live server does not advertise the tool that was called.
    pub const UNKNOWN_TOOL: &str = "unknown-tool";
}

/// A failure of one MCP server, or of one call to it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpFault {
    #[error("the handshake with the MCP server failed: {0}")]
    Handshake(String),

    #[error("the MCP server did not answer {method:?} within {}ms", after.as_millis())]
    Timeout { method: String, after: Duration },

    #[error("the connection to the MCP server ended: {0}")]
    Transport(String),

    #[error("the MCP server sent something that is not a message: {0}")]
    Protocol(String),

    #[error("the MCP server refused {method:?}: {message} (code {code})")]
    Server {
        method: String,
        code: i64,
        message: String,
    },

    #[error("the MCP tool {name:?} reported failure: {message}")]
    Tool { name: String, message: String },

    #[error("no live connection to the MCP server: {0}")]
    Unavailable(String),

    #[error("the MCP server does not advertise a tool called {0:?}")]
    UnknownTool(String),
}

impl McpFault {
    /// The one word this failure is classified by.
    pub fn class(&self) -> &'static str {
        match self {
            Self::Handshake(_) => class::HANDSHAKE,
            Self::Timeout { .. } => class::TIMEOUT,
            Self::Transport(_) => class::TRANSPORT,
            Self::Protocol(_) => class::PROTOCOL,
            Self::Server { .. } => class::SERVER,
            Self::Tool { .. } => class::TOOL,
            Self::Unavailable(_) => class::UNAVAILABLE,
            Self::UnknownTool(_) => class::UNKNOWN_TOOL,
        }
    }

    /// Whether the connection itself is gone, so a supervisor should replace
    /// it rather than let the next call find out the same way.
    ///
    /// A server that refused a call is answering, so restarting it changes
    /// nothing and would only cost the tools of every other call in flight.
    /// A call that timed out is deliberately *not* here: one slow tool must
    /// not take down a server the other tools are still using, so the call is
    /// cancelled and the connection is left alone.
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::Protocol(_))
    }
}
