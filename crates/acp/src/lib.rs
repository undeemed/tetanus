//! The Agent Client Protocol bridge.
//!
//! [ACP](https://agentclientprotocol.com) is how an editor drives an agent it
//! did not write. It is JSON-RPC 2.0 over stdio, one object per line, which is
//! exactly the framing `crates/rpc` already carries - so this crate implements
//! [`tetanus_rpc::FrameHandler`] and rides that carrier rather than bringing a
//! second one. Line framing, concurrent dispatch, the writer task, and the
//! promise that a notification written during a call reaches the peer before
//! that call's answer are the carrier's properties, and having them in one
//! place is what keeps them true of both protocols.
//!
//! What crosses, and what does not:
//!
//! | ACP | tetanus |
//! | --- | --- |
//! | `initialize` | connection state; a single-version agent, no auth methods, no prompt capabilities |
//! | `authenticate` | a no-op, because no authentication method was advertised |
//! | `session/new` | `session.create`, with the engine's own id handed back |
//! | `session/prompt` | `session.subscribe`, then `agent.prompt`, then unsubscribe |
//! | `session/cancel` | `agent.interrupt`, and the prompt settles `cancelled` |
//! | `session/update` | committed `assistant/message`, `tool/call`, `tool/result` |
//! | `session/request_permission` | one-shot allow or reject, fail-closed |
//!
//! Two deliberate departures from upstream's bridge, both recorded in
//! `docs/parity-updates/acp-bridge.md`.
//!
//! Tool activity **is** on this wire. Upstream keeps it off its automation
//! wire; ACP has first-class `tool_call` and `tool_call_update` variants, and
//! this workspace's contract already says the journal is the stream
//! (`docs/interface-contract.md` §7.2), so a second quieter history for one
//! client would be the thing §7.2 exists to reject.
//!
//! Images are refused rather than stored. An image prompt needs a durable
//! attachment store to refer to, and this workspace has none yet, so
//! `initialize` advertises `image: false` and a prompt carrying one is refused
//! by name - at the door, where a client can adapt, rather than mid-turn.
//!
//! What is not here at all: loading, listing, resuming, forking and deleting
//! sessions; editor navigation; modes; plans; titles. Upstream's bridge omits
//! them too, and each is a phase ②/③ line in `docs/parity.md`.

//! ## Both halves
//!
//! [`AcpBridge`] is the agent; [`AcpClient`] is a peer that spawns one and
//! drives it over real pipes. The client is here rather than in a suite
//! because a protocol whose only consumer is a test double is a shape nobody
//! has exercised: the failures that matter - an unanswered
//! `session/request_permission`, a child that stops speaking, frames
//! interleaved on one pipe - appear only when a second process is on the other
//! end.

pub mod bridge;
pub mod client;
pub mod content;
pub mod updates;
pub mod wire;

pub use bridge::AcpBridge;
pub use client::{AcpClient, ClientError, Launch, PermissionPolicy, PromptOutcome};
pub use content::{admit, ContentError};
pub use updates::updates_of;
pub use wire::{ContentBlock, SessionUpdate, StopReason, PROTOCOL_VERSION};

use std::sync::Arc;

/// Serve one ACP connection over the workspace's stdio carrier.
///
/// Returns when the peer stops writing, every frame in flight has been
/// answered, and every answer has been written.
pub async fn serve<R, W>(
    engine: Arc<dyn tetanus_protocol::methods::Engine>,
    input: R,
    output: W,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let bridge: Arc<dyn tetanus_rpc::FrameHandler> = Arc::new(AcpBridge::new(engine));
    tetanus_rpc::stdio::serve_handler(bridge, input, output).await
}
