//! What the MCP cases talk to: a scripted server on a channel pair, and the
//! real fixture program on a real pipe.
//!
//! Both exist on purpose. A channel pair pins what the client does with a
//! *message* - an answer out of order, a line that is not JSON, a cursor that
//! repeats - with no timing in the case at all. The fixture program pins what
//! it does with a *process* - an exit status, a pipe that ends, a child that
//! will not leave. Neither substitutes for the other.

#![allow(dead_code)]

use std::time::Duration;

use serde_json::{json, Value};
use tetanus_mcp::memory::Peer;
use tetanus_mcp::{ClientInfo, McpClient, ServerCommand, Timeouts};

/// The fixture server, in the named mode. `serve` is a correct server.
pub fn fixture(mode: &str) -> ServerCommand {
    ServerCommand::new(env!("CARGO_BIN_EXE_tetanus-mcp-fixture"))
        .env("TETANUS_MCP_FIXTURE", mode)
        // Short, because two cases wait for it and neither is about patience.
        .grace(Duration::from_millis(300))
}

/// Connect to the fixture server in the named mode.
pub async fn connect_fixture(
    mode: &str,
    timeouts: Timeouts,
) -> Result<McpClient, tetanus_mcp::McpFault> {
    let link = fixture(mode).spawn().expect("the fixture server starts");
    McpClient::connect("fixture", link, timeouts, ClientInfo::default()).await
}

/// Budgets short enough that a case about a server that never answers finishes
/// in well under a second.
pub fn brisk() -> Timeouts {
    Timeouts {
        handshake: Duration::from_millis(400),
        request: Duration::from_millis(400),
    }
}

/// Drive a scripted server on the other end of a channel pair.
///
/// `answer` is handed every message the client sent and returns the lines to
/// write back, so a case can answer out of order, answer twice, or answer with
/// something that is not a message at all.
pub fn scripted<F>(mut peer: Peer, mut answer: F) -> tokio::task::JoinHandle<()>
where
    F: FnMut(&Value) -> Vec<String> + Send + 'static,
{
    tokio::spawn(async move {
        while let Some(line) = peer.recv().await {
            let message: Value = serde_json::from_str(&line).expect("the client sends JSON");
            for reply in answer(&message) {
                if !peer.send(reply) {
                    return;
                }
            }
        }
    })
}

/// The handshake answer a correct server gives.
pub fn hello(id: &Value) -> String {
    result(
        id,
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": { "listChanged": true } },
            "serverInfo": { "name": "scripted", "version": "9.9.9" },
        }),
    )
}

/// A `result` frame.
pub fn result(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

/// An `error` frame.
pub fn refusal(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

/// The id of a request the client sent.
pub fn id_of(message: &Value) -> Value {
    message.get("id").cloned().unwrap_or(Value::Null)
}

/// The method of a message the client sent.
pub fn method_of(message: &Value) -> String {
    message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Whether a process is still on this system. Used to prove no child is left
/// behind; `/proc` is the only reading that does not depend on this process
/// still being the parent.
#[cfg(target_os = "linux")]
pub fn process_exists(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}
