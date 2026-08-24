//! The MCP conversation itself: the handshake, tool discovery, tool
//! invocation, and a clean end.
//!
//! Everything here is layered on [`Connection`], which knows nothing about
//! MCP beyond JSON-RPC. That split is what lets the protocol be tested against
//! a channel pair and the process handling be tested against a real child.
//!
//! **The handshake is not optional and not lenient about its own version.**
//! A server that answers with a revision this client has never seen is refused
//! at the handshake, because the alternative is discovering the difference
//! later, in the shape of a tool result.
//!
//! **Discovery drains pages, and refuses a server that will not stop.** A
//! cursor that repeats, or a page count past the cap, is a server bug that
//! would otherwise be an unbounded loop inside a boot.
//!
//! **A result is text, plus a stated placeholder for what is not.** tetanus's
//! tool outcome is text ([`tetanus_turn::tools::ToolOutcome`]), so an image or
//! an audio block cannot be carried to the model yet. It is reported as the
//! kind of block it was rather than dropped silently - upstream's
//! "reports unsupported audio without claiming the raw block was discarded" -
//! and the durable attachment route that would carry it is a `docs/parity.md`
//! row.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};

use crate::connection::Connection;
use crate::fault::McpFault;
use crate::link::{Departure, Link};
use crate::wire::{self, method};

/// How long each phase of the conversation may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// The `initialize` round trip. Short: a server that cannot say hello
    /// promptly is a server whose tools nobody should be waiting on.
    pub handshake: Duration,
    /// Any later request, including one tool call.
    pub request: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(10),
            // Upstream's default tool-call budget.
            request: Duration::from_secs(30),
        }
    }
}

/// Who this client says it is. A server may log it, and some gate behaviour on
/// it, so it is stated rather than left blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

impl Default for ClientInfo {
    fn default() -> Self {
        Self {
            name: "tetanus".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// What the server said about itself in the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    /// The revision the server answered with, which may not be the one asked
    /// for - it is in [`wire::SUPPORTED_VERSIONS`] or the handshake failed.
    pub protocol_version: String,
    /// Whether the server declared a `tools` capability at all.
    pub serves_tools: bool,
    /// Whether it promises to notify when its tool list changes.
    pub list_changed: bool,
}

/// One tool a server advertises.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDescription {
    /// The name that goes on the wire. Never shown to the model.
    pub raw_name: String,
    pub description: String,
    /// The JSON Schema for the tool's arguments, as the server wrote it.
    pub input_schema: Value,
}

/// What a tool call produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolAnswer {
    /// Every text block, joined by newlines, plus a stated placeholder for
    /// each block this build cannot carry.
    pub text: String,
    /// The server's own `structuredContent`, when it sent one.
    pub structured: Option<Value>,
}

/// Most pages a discovery will read before it decides the server is broken.
const MAX_TOOL_PAGES: usize = 100;

/// A connected, initialized MCP server.
#[derive(Debug)]
pub struct McpClient {
    connection: Connection,
    server: ServerInfo,
    timeouts: Timeouts,
}

impl McpClient {
    /// Connect over `link` and complete the handshake.
    ///
    /// A failed handshake closes the link before returning: a server that was
    /// started and not talked to is still a process, and leaving it running
    /// would be the orphan this crate promises not to make.
    pub async fn connect(
        server_name: impl Into<String>,
        link: Link,
        timeouts: Timeouts,
        client: ClientInfo,
    ) -> Result<Self, McpFault> {
        let connection = Connection::open(server_name, link);
        match handshake(&connection, &timeouts, &client).await {
            Ok(server) => Ok(Self {
                connection,
                server,
                timeouts,
            }),
            Err(fault) => {
                connection.close().await;
                Err(fault)
            }
        }
    }

    pub fn server(&self) -> &ServerInfo {
        &self.server
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Every tool the server advertises, in the order it advertised them.
    pub async fn list_tools(&self) -> Result<Vec<ToolDescription>, McpFault> {
        let mut found: Vec<ToolDescription> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen: BTreeSet<String> = BTreeSet::new();

        for page in 0..MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let answer = self
                .connection
                .call(method::TOOLS_LIST, params, self.timeouts.request)
                .await?;
            let listed = answer
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    protocol(
                        &self.server.name,
                        "a tools/list result carries a `tools` array",
                    )
                })?;
            for tool in listed {
                found.push(described(&self.server.name, tool)?);
            }
            let next = answer
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            match next {
                None => return Ok(found),
                Some(next) if !seen.insert(next.clone()) => {
                    return Err(protocol(
                        &self.server.name,
                        &format!("tools/list page {page} handed back a cursor it had already sent"),
                    ))
                }
                Some(next) => cursor = Some(next),
            }
        }
        Err(protocol(
            &self.server.name,
            &format!("tools/list never ran out of pages ({MAX_TOOL_PAGES} read)"),
        ))
    }

    /// Call one tool by the name the server advertised.
    ///
    /// A tool that reports `isError` is a [`McpFault::Tool`]: the tool ran and
    /// said no, which is a different thing from the server failing, and the
    /// class says which.
    pub async fn call_tool(
        &self,
        raw_name: &str,
        arguments: &Value,
    ) -> Result<ToolAnswer, McpFault> {
        let answer = self
            .connection
            .call(
                method::TOOLS_CALL,
                json!({ "name": raw_name, "arguments": arguments }),
                self.timeouts.request,
            )
            .await?;
        let text = rendered(&self.server.name, &answer)?;
        if answer
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(McpFault::Tool {
                name: raw_name.to_string(),
                message: text,
            });
        }
        Ok(ToolAnswer {
            text,
            structured: answer.get("structuredContent").cloned(),
        })
    }

    /// End the conversation and stop the server.
    pub async fn close(&self) -> Departure {
        self.connection.close().await
    }
}

async fn handshake(
    connection: &Connection,
    timeouts: &Timeouts,
    client: &ClientInfo,
) -> Result<ServerInfo, McpFault> {
    let server_name = connection.server().to_string();
    let answer = connection
        .call(
            method::INITIALIZE,
            json!({
                "protocolVersion": wire::PROTOCOL_VERSION,
                // No client capability is declared, because none is served:
                // this client offers no sampling, no roots and no elicitation,
                // and saying otherwise would invite requests it refuses.
                "capabilities": {},
                "clientInfo": { "name": client.name, "version": client.version },
            }),
            timeouts.handshake,
        )
        .await
        // Every way this can fail is one thing to the caller - the server
        // never came up - and the original words are kept inside it.
        .map_err(|fault| McpFault::Handshake(fault.to_string()))?;

    let protocol_version = answer
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            McpFault::Handshake(format!(
                "{server_name}: the initialize result carries no protocolVersion"
            ))
        })?
        .to_string();
    if !wire::SUPPORTED_VERSIONS.contains(&protocol_version.as_str()) {
        return Err(McpFault::Handshake(format!(
            "{server_name}: speaks MCP {protocol_version}; this client speaks {}",
            wire::SUPPORTED_VERSIONS.join(", ")
        )));
    }

    let tools = answer
        .get("capabilities")
        .and_then(|caps| caps.get("tools"));
    let server = ServerInfo {
        name: answer
            .pointer("/serverInfo/name")
            .and_then(Value::as_str)
            .unwrap_or(&server_name)
            .to_string(),
        version: answer
            .pointer("/serverInfo/version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        protocol_version,
        serves_tools: tools.is_some(),
        list_changed: tools
            .and_then(|tools| tools.get("listChanged"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };

    // The server may not answer anything until this arrives, so a failure to
    // send it is a failed handshake rather than something to find out later.
    connection
        .notify(method::INITIALIZED, json!({}))
        .await
        .map_err(|fault| McpFault::Handshake(fault.to_string()))?;
    Ok(server)
}

fn described(server: &str, tool: &Value) -> Result<ToolDescription, McpFault> {
    let raw_name = tool
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| protocol(server, "an advertised tool carries a name"))?;
    Ok(ToolDescription {
        raw_name: raw_name.to_string(),
        description: tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        // A server that advertises no schema advertises a tool with no
        // arguments: the model needs a shape, and an absent one is not a
        // reason to drop the tool.
        input_schema: tool
            .get("inputSchema")
            .filter(|schema| schema.is_object())
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
    })
}

/// A tool result's content blocks, as the one string a tetanus tool answers.
fn rendered(server: &str, answer: &Value) -> Result<String, McpFault> {
    let Some(blocks) = answer.get("content") else {
        // A result with no content at all is legal and means the tool said
        // nothing; an empty string is the honest rendering of that.
        return Ok(String::new());
    };
    let blocks = blocks
        .as_array()
        .ok_or_else(|| protocol(server, "a tool result's `content` is an array of blocks"))?;
    let mut lines: Vec<String> = Vec::new();
    for block in blocks {
        lines.push(block_text(block));
    }
    Ok(lines.join("\n"))
}

/// One content block as text. Anything this build cannot carry is named rather
/// than dropped, so a reader knows something was there.
fn block_text(block: &Value) -> String {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            // A text block with no text is a server bug worth seeing.
            .unwrap_or("[a text block with no text]")
            .to_string(),
        Some(kind @ ("image" | "audio")) => {
            let media = block
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("an unstated type");
            format!("[{kind} of {media}: this build carries no attachment store, so the block was not passed on]")
        }
        Some("resource_link") => {
            let uri = block.get("uri").and_then(Value::as_str).unwrap_or("no uri");
            let name = block.get("name").and_then(Value::as_str).unwrap_or("");
            format!("[resource {name} at {uri}]")
        }
        Some("resource") => {
            let uri = block
                .pointer("/resource/uri")
                .and_then(Value::as_str)
                .unwrap_or("no uri");
            format!("[embedded resource at {uri}]")
        }
        Some(other) => format!("[a {other} block, which this build does not carry]"),
        None => "[a block with no type]".to_string(),
    }
}

fn protocol(server: &str, what: &str) -> McpFault {
    McpFault::Protocol(format!("{server}: {what}"))
}
