//! The JSON-RPC 2.0 frames MCP speaks, and the newline framing stdio carries
//! them in.
//!
//! One message per line, no embedded newlines: `serde_json::to_string` never
//! writes one, and a line that will not parse is a framing failure rather than
//! a message to skip - see the crate note on why nothing resynchronises.
//!
//! This module knows nothing about tools or handshakes. It is the layer that
//! turns text into one of the four things a peer can send, so
//! [`crate::connection`] can route without parsing and [`crate::client`] can
//! parse without framing.

use serde_json::{json, Value};

/// The protocol revision this client asks for.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The revisions this client will keep talking to when a server answers the
/// handshake with one of its own.
///
/// A server is allowed to answer with a revision other than the one asked for.
/// Accepting anything at all would mean speaking a protocol whose shapes this
/// code has never seen; refusing everything but [`PROTOCOL_VERSION`] would
/// break against every server that has not upgraded yet. The list is the
/// middle: the revisions whose `initialize`, `tools/list` and `tools/call`
/// shapes are the ones served here.
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// The methods this client sends and the notifications it listens for.
pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const INITIALIZED: &str = "notifications/initialized";
    pub const CANCELLED: &str = "notifications/cancelled";
    pub const TOOLS_LIST: &str = "tools/list";
    pub const TOOLS_CALL: &str = "tools/call";
    /// The server telling the client its tool list has changed.
    pub const TOOL_LIST_CHANGED: &str = "notifications/tools/list_changed";
}

/// JSON-RPC's own code for a method the peer does not serve. Sent when a
/// server asks *this* process for something: a request left unanswered stalls
/// a server that waits for it, so refusal is the containing answer.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// A JSON-RPC error object, as a server sends it.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub code: i64,
    pub message: String,
}

/// One thing a peer sent.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// An answer to a request this client made.
    Answer {
        id: u64,
        outcome: Result<Value, Refusal>,
    },
    /// A notification: no answer is owed.
    Notification { method: String, params: Value },
    /// A request from the server to this client.
    Ask { id: Value, method: String },
}

/// Read one line as a frame, or say why it is not one.
///
/// The message names what arrived, cut short, because the first question about
/// a server that speaks nonsense is what the nonsense was - and the second is
/// whether it printed a log line to stdout, which is the single most common
/// way an MCP server is broken.
pub fn parse(line: &str) -> Result<Frame, String> {
    let value: Value = serde_json::from_str(line)
        .map_err(|source| format!("{source} in {:?}", cut(line.trim(), 200)))?;
    let Some(object) = value.as_object() else {
        return Err(format!(
            "a message is a JSON object, not {}",
            kind_of(&value)
        ));
    };
    let id = object.get("id");
    let method = object.get("method").and_then(Value::as_str);

    match (method, id) {
        // A request from the server. Its id is echoed back untouched, whatever
        // shape it has, because that is the only thing the server can match.
        (Some(method), Some(id)) if !id.is_null() => Ok(Frame::Ask {
            id: id.clone(),
            method: method.to_string(),
        }),
        (Some(method), _) => Ok(Frame::Notification {
            method: method.to_string(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        }),
        (None, Some(id)) => {
            let id = id.as_u64().ok_or_else(|| {
                format!(
                    "an answer carries the id it answers; this one carries {}",
                    kind_of(id)
                )
            })?;
            let outcome = match object.get("error") {
                Some(error) => Err(refusal(error)),
                None => Ok(object.get("result").cloned().unwrap_or(Value::Null)),
            };
            Ok(Frame::Answer { id, outcome })
        }
        (None, None) => {
            Err("a message carries a method or an id; this one has neither".to_string())
        }
    }
}

/// A request, framed.
pub fn request(id: u64, method: &str, params: Value) -> String {
    line(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
}

/// A notification, framed. No id, so no answer is owed and none is waited for.
pub fn notification(method: &str, params: Value) -> String {
    line(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
}

/// The refusal this client sends when a server asks it for something.
pub fn unsupported(id: &Value, method: &str) -> String {
    line(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": METHOD_NOT_FOUND,
            "message": format!("this MCP client serves no {method:?}"),
        },
    }))
}

fn line(value: Value) -> String {
    // Serializing a `Value` cannot fail: it holds no map with non-string keys
    // and no float that is not a number.
    serde_json::to_string(&value).expect("a JSON value serializes")
}

fn refusal(error: &Value) -> Refusal {
    Refusal {
        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the server gave no message")
            .to_string(),
    }
}

/// What a value is, for a message about a value that is the wrong thing.
fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The first `bound` characters, with an ellipsis when there were more. Cut on
/// a character boundary, because a server that sends nonsense can send
/// multi-byte nonsense.
fn cut(text: &str, bound: usize) -> String {
    match text.char_indices().nth(bound) {
        None => text.to_string(),
        Some((at, _)) => format!("{}...", &text[..at]),
    }
}
