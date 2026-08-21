//! Test Design Specification: the MCP client's half of the protocol, ported.
//!
//! Feature under test: `tetanus_mcp::client` and `tetanus_mcp::connection` -
//! the handshake, tool discovery, tool invocation, and what happens when the
//! server on the other end is wrong. Upstream pins the same behaviour in
//! `packages/mcp/mcp-client/tests/mcp-client.spec.ts`; each case names the
//! upstream case it restates.
//!
//! Approach: a scripted server on a channel pair. Every case here is about a
//! *message*, so a case that spawned a process would be spending a process to
//! assert a string. The process cases are `stdio_server.rs`, and they are
//! about the things a channel pair cannot be wrong about.
//!
//! What is not restated, and why. Upstream's image and audio admission - a
//! durable attachment store, an admission policy, a route that declares image
//! input - has nothing behind it here: a tetanus tool outcome is text, so the
//! restatement is that an unsupported block is *named* rather than dropped
//! (TC-PORT-MCP-13), and the attachment route stays a `docs/parity.md` row.
//! Upstream's Cordis plugin lifecycle (load-path guards, `unwrapExports`, HMR
//! disposers) has no counterpart in a compile-time registry. Its
//! streamable-HTTP transport is one row in the same table.
//!
//! Environmental needs: none. No case reaches a network, an API key, or a
//! filesystem.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::time::Duration;

use serde_json::{json, Value};
use tetanus_mcp::fault::class;
use tetanus_mcp::wire::method;
use tetanus_mcp::{memory, ClientInfo, McpClient, McpFault, Timeouts};

use harness::{hello, id_of, method_of, refusal, result, scripted};

/// TC-PORT-MCP-1: the handshake states who is calling, and the initialized
/// notification follows the answer.
///
/// Upstream: "connects, syncs tools under the namespace, and registers a
/// notification handler".
///
/// A server may not serve anything until `notifications/initialized` arrives,
/// so a client that skipped it would hang on its first real request instead of
/// failing at the handshake.
///
/// Input: a scripted server answering `initialize` correctly.
/// Expected: the client sends `initialize` carrying a protocol version and its
/// own name, then `notifications/initialized`, and reports the server's name,
/// version and revision.
#[tokio::test]
async fn the_handshake_states_who_is_calling_and_is_completed_by_a_notification() {
    let (link, peer) = memory::pair();
    let (seen_tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
    scripted(peer, move |message| {
        let _ = seen_tx.send(message.clone());
        match method_of(message).as_str() {
            method::INITIALIZE => vec![hello(&id_of(message))],
            _ => vec![],
        }
    });

    let client = McpClient::connect(
        "scripted",
        link,
        Timeouts::default(),
        ClientInfo {
            name: "tetanus".into(),
            version: "1.2.3".into(),
        },
    )
    .await
    .expect("the handshake completes");

    let initialize = seen.recv().await.expect("initialize was sent");
    assert_eq!(method_of(&initialize), method::INITIALIZE);
    assert_eq!(
        initialize.pointer("/params/protocolVersion"),
        Some(&json!(tetanus_mcp::wire::PROTOCOL_VERSION)),
    );
    assert_eq!(
        initialize.pointer("/params/clientInfo"),
        Some(&json!({ "name": "tetanus", "version": "1.2.3" })),
    );

    let initialized = seen.recv().await.expect("initialized was sent");
    assert_eq!(method_of(&initialized), method::INITIALIZED);
    assert_eq!(initialized.get("id"), None, "a notification carries no id");

    assert_eq!(client.server().name, "scripted");
    assert_eq!(client.server().version, "9.9.9");
    assert_eq!(client.server().protocol_version, "2025-06-18");
    assert!(client.server().serves_tools);
    assert!(client.server().list_changed);
}

/// TC-PORT-MCP-2: a revision this client does not speak fails the handshake.
///
/// Upstream: its SDK refuses an unsupported `protocolVersion` at initialize.
///
/// The alternative is finding out later, in the shape of a tool result that
/// does not parse, with a turn already running.
///
/// Input: a server answering the handshake with `1999-01-01`.
/// Expected: `McpFault::Handshake`, class `handshake`, naming the revision.
#[tokio::test]
async fn a_protocol_revision_this_client_does_not_speak_fails_the_handshake() {
    let (link, peer) = memory::pair();
    scripted(peer, |message| match method_of(message).as_str() {
        method::INITIALIZE => vec![result(
            &id_of(message),
            json!({ "protocolVersion": "1999-01-01", "capabilities": {} }),
        )],
        _ => vec![],
    });

    let fault = McpClient::connect("scripted", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect_err("the handshake is refused");
    assert_eq!(fault.class(), class::HANDSHAKE);
    assert!(
        fault.to_string().contains("1999-01-01"),
        "the message names the revision: {fault}"
    );
}

/// TC-PORT-MCP-3: discovery drains every page, in the server's order.
///
/// Upstream: "drains paginated listTools responses".
///
/// A client that read one page would offer the model a tool set that is a
/// prefix of the truth, and the missing tools would look like server bugs.
///
/// Input: a server answering `tools/list` in two pages joined by a cursor.
/// Expected: both pages' tools, in the order they were advertised, and the
/// second request carries the cursor from the first answer.
#[tokio::test]
async fn discovery_drains_every_page_in_the_order_the_server_gave() {
    let (link, peer) = memory::pair();
    let (cursor_tx, mut cursors) = tokio::sync::mpsc::unbounded_channel();
    scripted(peer, move |message| match method_of(message).as_str() {
        method::INITIALIZE => vec![hello(&id_of(message))],
        method::TOOLS_LIST => {
            let cursor = message
                .pointer("/params/cursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            let _ = cursor_tx.send(cursor.clone());
            match cursor.as_deref() {
                None => vec![result(
                    &id_of(message),
                    json!({
                        "tools": [{ "name": "zebra", "inputSchema": { "type": "object" } }],
                        "nextCursor": "page-2",
                    }),
                )],
                Some(_) => vec![result(
                    &id_of(message),
                    json!({ "tools": [{ "name": "aardvark", "description": "second" }] }),
                )],
            }
        }
        _ => vec![],
    });

    let client = McpClient::connect("scripted", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect("connected");
    let tools = client.list_tools().await.expect("listed");

    let names: Vec<&str> = tools.iter().map(|tool| tool.raw_name.as_str()).collect();
    assert_eq!(
        names,
        vec!["zebra", "aardvark"],
        "the server's order is kept, not sorted"
    );
    assert_eq!(cursors.recv().await.expect("first page"), None);
    assert_eq!(
        cursors.recv().await.expect("second page"),
        Some("page-2".to_string())
    );
    // A tool that advertised no schema still has a shape the model can read.
    assert_eq!(
        tools[1].input_schema,
        json!({ "type": "object", "properties": {} })
    );
}

/// TC-PORT-MCP-4: a cursor that repeats is refused rather than followed.
///
/// Upstream has no case for it; the shape it guards is upstream's own drain
/// loop, which a server can hold open for ever.
///
/// Discovery runs inside a boot. An unbounded loop there is a harness that
/// never starts, with no message saying why.
///
/// Input: a server that answers every `tools/list` with the same cursor.
/// Expected: `McpFault::Protocol`, class `protocol`, saying the cursor
/// repeated, in bounded time.
#[tokio::test]
async fn a_cursor_that_repeats_is_refused_rather_than_followed() {
    let (link, peer) = memory::pair();
    scripted(peer, |message| match method_of(message).as_str() {
        method::INITIALIZE => vec![hello(&id_of(message))],
        method::TOOLS_LIST => vec![result(
            &id_of(message),
            json!({ "tools": [{ "name": "one" }], "nextCursor": "always-the-same" }),
        )],
        _ => vec![],
    });

    let client = McpClient::connect("scripted", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect("connected");
    let fault = client.list_tools().await.expect_err("refused");
    assert_eq!(fault.class(), class::PROTOCOL);
    assert!(
        fault.to_string().contains("already sent"),
        "the message says what was wrong: {fault}"
    );
}

/// TC-PORT-MCP-5: a call sends the raw name and joins the text it got back.
///
/// Upstream: "calls MCP callTool with the RAW name and returns text content",
/// "joins multiple text blocks with newline", "preserves structuredContent on
/// a successful result".
///
/// Input: a call to `search` with arguments, answered with two text blocks and
/// a `structuredContent`.
/// Expected: the wire carries `search` and the arguments unchanged; the answer
/// is the two blocks joined by a newline, with the structured content kept.
#[tokio::test]
async fn a_call_sends_the_raw_name_and_joins_the_text_it_got_back() {
    let (link, peer) = memory::pair();
    let (sent_tx, mut sent) = tokio::sync::mpsc::unbounded_channel();
    scripted(peer, move |message| match method_of(message).as_str() {
        method::INITIALIZE => vec![hello(&id_of(message))],
        method::TOOLS_CALL => {
            let _ = sent_tx.send(message.get("params").cloned().unwrap_or(Value::Null));
            vec![result(
                &id_of(message),
                json!({
                    "content": [
                        { "type": "text", "text": "first" },
                        { "type": "text", "text": "second" },
                    ],
                    "structuredContent": { "hits": 2 },
                }),
            )]
        }
        _ => vec![],
    });

    let client = McpClient::connect("scripted", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect("connected");
    let answer = client
        .call_tool("search", &json!({ "query": "rust" }))
        .await
        .expect("the tool answered");

    assert_eq!(
        sent.recv().await.expect("the call went out"),
        json!({ "name": "search", "arguments": { "query": "rust" } }),
    );
    assert_eq!(answer.text, "first\nsecond");
    assert_eq!(answer.structured, Some(json!({ "hits": 2 })));
}

/// TC-PORT-MCP-6: a tool reporting `isError` is a tool failure, not a server
/// failure.
///
/// Upstream: "maps isError to an error result via throw".
///
/// The distinction is the whole point of the class: a tool that said no is
/// something the model can react to, and a server that is broken is something
/// the operator has to fix.
///
/// Input: a call answered with `isError: true` and a message.
/// Expected: `McpFault::Tool`, class `tool`, carrying the tool's own words.
#[tokio::test]
async fn a_tool_reporting_is_error_is_a_tool_failure_not_a_server_failure() {
    let (link, peer) = memory::pair();
    scripted(peer, |message| match method_of(message).as_str() {
        method::INITIALIZE => vec![hello(&id_of(message))],
        method::TOOLS_CALL => vec![result(
            &id_of(message),
            json!({
                "content": [{ "type": "text", "text": "the path does not exist" }],
                "isError": true,
            }),
        )],
        _ => vec![],
    });

    let client = McpClient::connect("scripted", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect("connected");
    let fault = client
        .call_tool("read", &json!({}))
        .await
        .expect_err("the tool refused");
    assert_eq!(fault.class(), class::TOOL);
    assert!(
        fault.to_string().contains("the path does not exist"),
        "the tool's own words survive: {fault}"
    );
}

/// TC-PORT-MCP-7: a JSON-RPC error names the call it refused.
///
/// Upstream: its SDK surfaces the error object; the addition here is that the
/// method is carried, because a server's message rarely says which call it is
/// about and a journal holds both.
///
/// Input: a `tools/call` answered with a JSON-RPC error.
/// Expected: `McpFault::Server`, class `server`, carrying the code, the
/// message, and the method that was refused.
#[tokio::test]
async fn a_json_rpc_error_names_the_call_it_refused() {
    let (link, peer) = memory::pair();
    scripted(peer, |message| match method_of(message).as_str() {
        method::INITIALIZE => vec![hello(&id_of(message))],
        method::TOOLS_CALL => vec![refusal(&id_of(message), -32602, "no such tool")],
        _ => vec![],
    });

    let client = McpClient::connect("scripted", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect("connected");
    let fault = client
        .call_tool("nope", &json!({}))
        .await
        .expect_err("refused");
    assert_eq!(
        fault,
        McpFault::Server {
            method: method::TOOLS_CALL.to_string(),
            code: -32602,
            message: "no such tool".to_string(),
        }
    );
    assert_eq!(fault.class(), class::SERVER);
}

/// TC-PORT-MCP-8: a line that is not a message ends the connection.
///
/// Upstream contains a malformed frame inside its SDK transport; the decision
/// restated here is tetanus's, and the crate note argues it: newline-delimited
/// JSON has no resynchronisation point, so a client that skipped the line
/// would be guessing where the next message begins.
///
/// Input: a server that writes a log line to stdout while a call is pending.
/// Expected: the pending call fails with class `protocol`, and the connection
/// reports itself no longer live.
#[tokio::test]
async fn a_line_that_is_not_a_message_ends_the_connection() {
    let (link, peer) = memory::pair();
    scripted(peer, |message| match method_of(message).as_str() {
        method::INITIALIZE => vec![hello(&id_of(message))],
        method::TOOLS_CALL => vec!["listening on port 8080".to_string()],
        _ => vec![],
    });

    let client = McpClient::connect("scripted", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect("connected");
    let fault = client
        .call_tool("anything", &json!({}))
        .await
        .expect_err("the call fails");
    assert_eq!(fault.class(), class::PROTOCOL);
    assert!(
        fault.to_string().contains("listening on port 8080"),
        "the message quotes what arrived: {fault}"
    );
    client.connection().departed().await;
    assert!(!client.connection().is_live());
}

/// TC-PORT-MCP-9: a peer that goes away fails everything waiting on it.
///
/// Upstream: "a transport that closes during a resolving connect registers
/// nothing from the dead generation" - the same rule, that a lost transport is
/// not something a pending call waits out.
///
/// Input: two calls in flight when the peer hangs up.
/// Expected: both fail with class `transport`, and the connection stops being
/// live.
#[tokio::test]
async fn a_peer_that_goes_away_fails_everything_waiting_on_it() {
    let (link, peer) = memory::pair();
    let (hangup_tx, hangup) = tokio::sync::oneshot::channel();
    let mut hangup_tx = Some(hangup_tx);
    let mut peer = Some(peer);
    let handle = tokio::spawn(async move {
        let mut owned = peer.take().expect("peer");
        while let Some(line) = owned.recv().await {
            let message: Value = serde_json::from_str(&line).expect("json");
            if method_of(&message) == method::INITIALIZE {
                owned.send(hello(&id_of(&message)));
            } else {
                // Both calls are in flight; drop everything.
                let _ = hangup_tx.take().map(|tx| tx.send(()));
                break;
            }
        }
        drop(owned);
    });

    let client = McpClient::connect("scripted", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect("connected");
    let empty = json!({});
    let one = client.call_tool("a", &empty);
    let two = client.call_tool("b", &empty);
    let (one, two) = tokio::join!(one, two);
    let _ = hangup.await;
    handle.await.expect("the scripted server finished");

    for fault in [one.expect_err("a failed"), two.expect_err("b failed")] {
        assert_eq!(fault.class(), class::TRANSPORT, "{fault}");
    }
    assert!(!client.connection().is_live());
}

/// TC-PORT-MCP-10: answers are matched by id, not by arrival order.
///
/// Upstream relies on its SDK for this; tetanus owns the pending map, so the
/// property is asserted where it lives. The tool pipeline dispatches
/// parallel-safe calls at once, so out-of-order answers are the normal case,
/// not the exotic one.
///
/// Input: three calls answered in reverse order.
/// Expected: each caller gets the answer to its own call.
#[tokio::test]
async fn answers_are_matched_by_id_not_by_arrival_order() {
    let (link, peer) = memory::pair();
    let mut held: Vec<(Value, String)> = Vec::new();
    scripted(peer, move |message| match method_of(message).as_str() {
        method::INITIALIZE => vec![hello(&id_of(message))],
        method::TOOLS_CALL => {
            let name = message
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            held.push((id_of(message), name));
            if held.len() < 3 {
                return vec![];
            }
            held.drain(..)
                .rev()
                .map(|(id, name)| {
                    result(
                        &id,
                        json!({ "content": [{ "type": "text", "text": format!("answer to {name}") }] }),
                    )
                })
                .collect()
        }
        _ => vec![],
    });

    let client = McpClient::connect("scripted", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect("connected");
    let empty = json!({});
    let (one, two, three) = tokio::join!(
        client.call_tool("one", &empty),
        client.call_tool("two", &empty),
        client.call_tool("three", &empty),
    );
    assert_eq!(one.expect("one answered").text, "answer to one");
    assert_eq!(two.expect("two answered").text, "answer to two");
    assert_eq!(three.expect("three answered").text, "answer to three");
}

/// TC-PORT-MCP-11: a request from the server is refused, not ignored.
///
/// Upstream declares client capabilities and its SDK answers what it can;
/// tetanus declares none, so every server request is refused - and refusing is
/// the point, because a server waiting on an answer that never comes is a
/// server that stops serving tools.
///
/// Input: a server sending a `sampling/createMessage` request.
/// Expected: an error frame with JSON-RPC's method-not-found code, carrying
/// the id the server used, and the connection stays live.
#[tokio::test]
async fn a_request_from_the_server_is_refused_not_ignored() {
    let (link, peer) = memory::pair();
    let (answered_tx, mut answered) = tokio::sync::mpsc::unbounded_channel();
    scripted(peer, move |message| match method_of(message).as_str() {
        method::INITIALIZE => vec![
            hello(&id_of(message)),
            json!({
                "jsonrpc": "2.0",
                "id": "server-side-id",
                "method": "sampling/createMessage",
                "params": {},
            })
            .to_string(),
        ],
        // The refusal comes back as a message with an id and no method.
        _ if message.get("method").is_none() => {
            let _ = answered_tx.send(message.clone());
            vec![]
        }
        _ => vec![],
    });

    let client = McpClient::connect("scripted", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect("connected");
    let refused = tokio::time::timeout(Duration::from_secs(5), answered.recv())
        .await
        .expect("the refusal was sent")
        .expect("a frame");
    assert_eq!(refused.get("id"), Some(&json!("server-side-id")));
    assert_eq!(
        refused.pointer("/error/code"),
        Some(&json!(tetanus_mcp::wire::METHOD_NOT_FOUND))
    );
    assert!(client.connection().is_live());
}

/// TC-PORT-MCP-12: a call that runs out of budget fails alone.
///
/// Upstream: "passes abort signal to callTool" - the same containment, reached
/// by a budget rather than by a caller's signal.
///
/// One slow tool must not cost every other tool its server, so the timeout
/// fails the call, tells the server to forget it, and leaves the connection
/// up for the next one.
///
/// Input: a server that never answers the first call and answers the second.
/// Expected: the first fails with class `timeout`; a `notifications/cancelled`
/// naming its request id goes out; the second call succeeds.
#[tokio::test]
async fn a_call_that_runs_out_of_budget_fails_alone() {
    let (link, peer) = memory::pair();
    let (seen_tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
    scripted(peer, move |message| {
        let _ = seen_tx.send(message.clone());
        match method_of(message).as_str() {
            method::INITIALIZE => vec![hello(&id_of(message))],
            method::TOOLS_CALL => {
                let name = message.pointer("/params/name").and_then(Value::as_str);
                match name {
                    Some("patient") => vec![],
                    _ => vec![result(
                        &id_of(message),
                        json!({ "content": [{ "type": "text", "text": "here" }] }),
                    )],
                }
            }
            _ => vec![],
        }
    });

    let timeouts = Timeouts {
        handshake: Duration::from_secs(5),
        request: Duration::from_millis(150),
    };
    let client = McpClient::connect("scripted", link, timeouts, ClientInfo::default())
        .await
        .expect("connected");

    let fault = client
        .call_tool("patient", &json!({}))
        .await
        .expect_err("the call times out");
    assert_eq!(fault.class(), class::TIMEOUT);

    let second = client
        .call_tool("prompt", &json!({}))
        .await
        .expect("the connection is still usable");
    assert_eq!(second.text, "here");
    assert!(client.connection().is_live());

    let mut cancelled = false;
    while let Ok(message) = seen.try_recv() {
        if method_of(&message) == method::CANCELLED {
            cancelled = true;
            assert!(
                message.pointer("/params/requestId").is_some(),
                "the cancellation names the request: {message}"
            );
        }
    }
    assert!(cancelled, "the server was told to forget the call");
}

/// TC-PORT-MCP-13: a block this build cannot carry is named, not dropped.
///
/// Upstream: "reports unsupported audio without claiming the raw block was
/// discarded", "handles unknown content types".
///
/// A tetanus tool outcome is text. Silently dropping an image would leave the
/// model reading a result that is missing the answer with no sign of it.
///
/// Input: a result carrying a text block, an image block and a block of an
/// invented type.
/// Expected: the text survives; each other block becomes a line naming what it
/// was and saying it was not passed on.
#[tokio::test]
async fn a_block_this_build_cannot_carry_is_named_not_dropped() {
    let (link, peer) = memory::pair();
    scripted(peer, |message| match method_of(message).as_str() {
        method::INITIALIZE => vec![hello(&id_of(message))],
        method::TOOLS_CALL => vec![result(
            &id_of(message),
            json!({
                "content": [
                    { "type": "text", "text": "the chart:" },
                    { "type": "image", "data": "AAAA", "mimeType": "image/png" },
                    { "type": "hologram" },
                ],
            }),
        )],
        _ => vec![],
    });

    let client = McpClient::connect("scripted", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect("connected");
    let answer = client
        .call_tool("chart", &json!({}))
        .await
        .expect("answered");
    let lines: Vec<&str> = answer.text.lines().collect();
    assert_eq!(lines[0], "the chart:");
    assert!(
        lines[1].contains("image") && lines[1].contains("image/png"),
        "the image is named: {:?}",
        lines[1]
    );
    assert!(
        lines[2].contains("hologram"),
        "an unknown block is named: {:?}",
        lines[2]
    );
}
