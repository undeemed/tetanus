//! A trivial MCP server, so the suite talks to a real program over a real
//! pipe instead of to a mock of one.
//!
//! It speaks the protocol properly by default, and misbehaves on demand: the
//! interesting cases in this crate are all about a server that is wrong, and a
//! server that is wrong on purpose is the only way to test them offline. What
//! it does is chosen by `TETANUS_MCP_FIXTURE`:
//!
//! | value | behaviour |
//! | --- | --- |
//! | unset, or `serve` | a correct server |
//! | `bad-version` | answers `initialize` with a protocol revision nobody speaks |
//! | `refuse-initialize` | answers `initialize` with a JSON-RPC error |
//! | `mute` | reads its input and never answers anything |
//! | `stubborn` | serves, but ignores end of input: only a kill stops it |
//!
//! The tools it advertises are named for what they do to the caller: `echo`
//! answers, `explode` reports `isError`, `hang` never answers, `crash` exits
//! the process mid-call, and `garbage` writes a line that is not a message.
//!
//! This is a `fixture`-feature binary. A published build does not carry it.

use std::io::{BufRead, Write};

fn main() {
    let mode = std::env::var("TETANUS_MCP_FIXTURE").unwrap_or_else(|_| "serve".to_string());
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let id = message.get("id").cloned();

        if mode == "mute" {
            continue;
        }

        let Some(id) = id else {
            // A notification. `notifications/initialized` is the one that
            // matters, and nothing is owed for any of them.
            continue;
        };

        match method.as_str() {
            "initialize" => answer(&mut out, &id, initialize(&mode)),
            "tools/list" => answer(&mut out, &id, tools()),
            "tools/call" => {
                let name = message
                    .pointer("/params/name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let arguments = message
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                match name.as_str() {
                    // Never answers. The caller's budget is what ends this.
                    "hang" => continue,
                    // Dies mid-call, with the request unanswered.
                    "crash" => std::process::exit(3),
                    "garbage" => {
                        let _ = writeln!(out, "this is not a message, it is a log line");
                        let _ = out.flush();
                    }
                    _ => answer(&mut out, &id, called(&name, &arguments)),
                }
            }
            other => refuse(&mut out, &id, &format!("no method {other:?}")),
        }
    }

    if mode == "stubborn" {
        // Input ended and this server does not care. Only a kill ends it.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
}

fn initialize(mode: &str) -> Result<serde_json::Value, String> {
    if mode == "refuse-initialize" {
        return Err("this server is not accepting connections today".to_string());
    }
    let version = if mode == "bad-version" {
        "1999-01-01"
    } else {
        "2025-06-18"
    };
    Ok(serde_json::json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": true } },
        "serverInfo": { "name": "tetanus-mcp-fixture", "version": "0.1.0" },
    }))
}

fn tools() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "tools": [
            {
                "name": "echo",
                "description": "Answer with the text it was given.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"],
                },
            },
            {
                "name": "explode",
                "description": "Report failure through isError.",
                "inputSchema": { "type": "object", "properties": {} },
            },
            {
                "name": "hang",
                "description": "Never answer.",
                "inputSchema": { "type": "object", "properties": {} },
            },
            {
                "name": "crash",
                "description": "Exit the server process mid-call.",
                "inputSchema": { "type": "object", "properties": {} },
            },
            {
                "name": "garbage",
                "description": "Write a line that is not a message.",
                "inputSchema": { "type": "object", "properties": {} },
            },
            {
                "name": "picture",
                "description": "Answer with an image block, which the client cannot carry.",
                "inputSchema": { "type": "object", "properties": {} },
            },
        ],
    }))
}

fn called(name: &str, arguments: &serde_json::Value) -> Result<serde_json::Value, String> {
    match name {
        "echo" => {
            let text = arguments
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            Ok(serde_json::json!({
                "content": [{ "type": "text", "text": format!("echo: {text}") }],
                "structuredContent": { "text": text },
            }))
        }
        "explode" => Ok(serde_json::json!({
            "content": [{ "type": "text", "text": "the tool decided against it" }],
            "isError": true,
        })),
        "picture" => Ok(serde_json::json!({
            "content": [
                { "type": "text", "text": "here it is" },
                { "type": "image", "data": "AAAA", "mimeType": "image/png" },
            ],
        })),
        other => Err(format!("no tool called {other:?}")),
    }
}

fn answer(
    out: &mut impl Write,
    id: &serde_json::Value,
    outcome: Result<serde_json::Value, String>,
) {
    match outcome {
        Ok(result) => {
            let frame = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result });
            let _ = writeln!(out, "{frame}");
            let _ = out.flush();
        }
        Err(message) => refuse(out, id, &message),
    }
}

fn refuse(out: &mut impl Write, id: &serde_json::Value, message: &str) {
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32601, "message": message },
    });
    let _ = writeln!(out, "{frame}");
    let _ = out.flush();
}
