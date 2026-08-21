//! The MCP client: the tools a Model Context Protocol server advertises,
//! served to the model through the same registry as tetanus's own.
//!
//! An MCP server is a program somebody else wrote, started by this process,
//! speaking JSON-RPC 2.0 over its standard input and output, one message per
//! line. Everything in this crate follows from that sentence:
//!
//! **The outside world is contained at the call, not at the turn.** A server
//! that dies, hangs, or answers something that is not a message fails the tool
//! call that was in flight and every call queued behind it, each with a class
//! the model and the operator can read ([`McpFault::class`]). The turn keeps
//! going, because a tool that failed is a result like any other -
//! `crates/turn/src/tools.rs` says the same thing about a tool that panics.
//!
//! **A line that is not a message ends the connection.** A framing this side
//! cannot parse means the stream is no longer trustworthy: there is no
//! resynchronisation point in newline-delimited JSON, so a client that skipped
//! the line would be guessing where the next message starts. The connection
//! goes down with [`McpFault::Protocol`] and the reconnect policy decides what
//! happens next.
//!
//! **Nothing this crate starts outlives it.** Every child is spawned with
//! `kill_on_drop`, closing a connection runs a bounded ladder - close the
//! child's input, wait out the grace period, then kill - and the departure is
//! reported rather than assumed, so a test can assert the child is gone.
//!
//! **The raw name goes on the wire; the public name goes to the model.** Two
//! servers may both advertise `search`, and a native tetanus tool may be
//! called `search` too, so a server's tools are published as
//! `mcp__<server>__<raw>` ([`tools::public_name`]). The public name is never
//! parsed to recover the raw one - the pair is carried, because normalisation
//! is lossy.
//!
//! Parity: upstream `packages/mcp/mcp-client`. Its transport list also carries
//! streamable HTTP, and its bridge admits image and audio blocks into a
//! durable attachment store; both are named in `docs/parity.md` rather than
//! implied here. What is restated is the client, the naming contract, the
//! bridge into the tool registry, and the reconnect supervisor.

pub mod client;
pub mod connection;
pub mod fault;
pub mod link;
pub mod memory;
pub mod stdio;
pub mod wire;

pub use client::{ClientInfo, McpClient, ServerInfo, Timeouts, ToolAnswer, ToolDescription};
pub use connection::{Connection, Notice};
pub use fault::McpFault;
pub use link::{Departure, Exit, Link};
pub use stdio::ServerCommand;
