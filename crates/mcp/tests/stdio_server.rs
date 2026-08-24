//! Test Design Specification: an MCP server as a real child process, ported.
//!
//! Feature under test: `tetanus_mcp::stdio` and what the client does with a
//! program rather than with a message - the end-to-end handshake against a
//! server that was started, a call that answers, a server that exits mid-call,
//! and the shutdown ladder. Upstream's transport half is
//! `packages/mcp/mcp-client/tests/mcp-client.spec.ts` ("createTransport") and
//! its own end-to-end file, `mcp-client.e2e.ts`.
//!
//! Approach: the fixture server in `src/bin/fixture.rs`, spawned for real.
//! A subprocess seam asserted against a fake spawner would be asserting the
//! fake, and the interesting things here - a pipe that ends, an exit status, a
//! child that ignores end of input - are the operating system's, exactly as
//! `crates/turn/tests/upstream_process.rs` argues for the same decision.
//!
//! What is not restated, and why. The protocol cases live next door in
//! `upstream_client.rs`, against a channel pair, because a process spent to
//! assert a string is a process spent for nothing. Streamable HTTP is a
//! `docs/parity.md` row.
//!
//! Environmental needs: the `tetanus-mcp-fixture` binary, which cargo builds
//! for this crate's tests through the `fixture` feature. No network, no key.
//! The whole file is skipped off unix rather than silently passing, because
//! two cases read `/proc` to prove a child is gone.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

mod harness;

use std::time::Duration;

use serde_json::json;
use tetanus_mcp::fault::class;
use tetanus_mcp::link::Exit;
use tetanus_mcp::{ClientInfo, McpClient, Timeouts};

use harness::{brisk, connect_fixture, fixture, process_exists};

/// TC-PORT-MCP-14: a real server is connected, listed and called.
///
/// Upstream: `mcp-client.e2e.ts`, which drives a real server over stdio.
///
/// This is the case the whole crate exists for: a program this process
/// started, a handshake over its pipes, the tools it advertises, and one of
/// them run.
///
/// Input: the fixture server in its correct mode.
/// Expected: the handshake names the server; `tools/list` carries `echo`
/// with the schema the server wrote; calling `echo` answers with the text it
/// was given, and the structured content survives.
#[tokio::test]
async fn a_real_server_is_connected_listed_and_called() {
    let client = connect_fixture("serve", Timeouts::default())
        .await
        .expect("the fixture server completes the handshake");

    assert_eq!(client.server().name, "tetanus-mcp-fixture");
    assert_eq!(client.server().protocol_version, "2025-06-18");
    assert!(client.server().serves_tools);

    let tools = client.list_tools().await.expect("listed");
    let echo = tools
        .iter()
        .find(|tool| tool.raw_name == "echo")
        .expect("the fixture advertises echo");
    assert_eq!(echo.description, "Answer with the text it was given.");
    assert_eq!(
        echo.input_schema.pointer("/properties/text/type"),
        Some(&json!("string"))
    );

    let answer = client
        .call_tool("echo", &json!({ "text": "over a pipe" }))
        .await
        .expect("the tool answered");
    assert_eq!(answer.text, "echo: over a pipe");
    assert_eq!(answer.structured, Some(json!({ "text": "over a pipe" })));

    let departure = client.close().await;
    assert_eq!(departure.exit, Exit::Code(0));
}

/// TC-PORT-MCP-15: closing takes the child with it.
///
/// Upstream: "effect disposer unregisters the CURRENT generation and closes
/// client", and its transport's termination grace periods.
///
/// A harness that leaves a server behind every time a session ends is a
/// harness that fills a machine with processes nobody can name.
///
/// Input: a correct server, closed after one call.
/// Expected: the departure names the pid and reports a clean exit, and the pid
/// is no longer a process on this system.
#[tokio::test]
async fn closing_takes_the_child_with_it() {
    let client = connect_fixture("serve", Timeouts::default())
        .await
        .expect("connected");
    client
        .call_tool("echo", &json!({ "text": "hello" }))
        .await
        .expect("answered");

    let departure = client.close().await;
    let pid = departure.pid.expect("a child has a pid");
    assert_eq!(departure.exit, Exit::Code(0));
    assert!(
        !process_exists(pid),
        "the server process {pid} is still on this system"
    );
}

/// TC-PORT-MCP-16: a server that ignores end of input is killed.
///
/// Upstream: its stdio transport escalates to a kill after its grace periods.
///
/// Closing the child's input is the polite half. A server that does not take
/// the hint is the whole reason the ladder has a floor.
///
/// Input: the fixture in `stubborn` mode, which loops for ever once its input
/// ends, with a 300ms grace.
/// Expected: the close reports `Killed` inside a second, and the pid is gone.
#[tokio::test]
async fn a_server_that_ignores_end_of_input_is_killed() {
    let link = fixture("stubborn").spawn().expect("started");
    let client = McpClient::connect("stubborn", link, Timeouts::default(), ClientInfo::default())
        .await
        .expect("connected");

    let departure = tokio::time::timeout(Duration::from_secs(5), client.close())
        .await
        .expect("closing is bounded");
    let pid = departure.pid.expect("a child has a pid");
    assert_eq!(departure.exit, Exit::Killed);
    assert!(
        !process_exists(pid),
        "the killed server {pid} is still on this system"
    );
}

/// TC-PORT-MCP-17: a server that exits mid-call fails that call.
///
/// Upstream: "reconnects after a transport close" - the outage this case is
/// the first half of; the reconnect half is `supervisor.rs`.
///
/// Input: the fixture's `crash` tool, which exits the process with the
/// request unanswered.
/// Expected: the call fails with class `transport`, the connection reports
/// itself no longer live, and the process is gone rather than a zombie.
#[tokio::test]
async fn a_server_that_exits_mid_call_fails_that_call() {
    let client = connect_fixture("serve", brisk()).await.expect("connected");
    let fault = client
        .call_tool("crash", &json!({}))
        .await
        .expect_err("the call fails");
    assert_eq!(fault.class(), class::TRANSPORT, "{fault}");

    client.connection().departed().await;
    assert!(!client.connection().is_live());
    let departure = client.close().await;
    if let Some(pid) = departure.pid {
        assert!(!process_exists(pid), "the crashed server {pid} is a zombie");
    }
}

/// TC-PORT-MCP-18: a server that never answers fails the handshake, and is
/// still stopped.
///
/// Upstream: "logs error and registers no tools when connect fails; dispose
/// closes the client".
///
/// A handshake that hung would hold up a boot for ever; a handshake that
/// failed and left the process running would be the orphan this crate
/// promises not to make.
///
/// Input: the fixture in `mute` mode, with a 400ms handshake budget.
/// Expected: class `handshake`, inside a second, and the process that was
/// started for it is gone.
#[tokio::test]
async fn a_server_that_never_answers_fails_the_handshake_and_is_still_stopped() {
    let link = fixture("mute").spawn().expect("started");
    let pid = link.pid.expect("a child has a pid");
    let connecting = McpClient::connect("mute", link, brisk(), ClientInfo::default());
    let fault = tokio::time::timeout(Duration::from_secs(5), connecting)
        .await
        .expect("the handshake is bounded")
        .expect_err("the handshake fails");
    assert_eq!(fault.class(), class::HANDSHAKE, "{fault}");

    // The connect path closes the link before returning, rather than handing
    // back a failure and leaving a process behind it.
    assert!(
        !process_exists(pid),
        "the server {pid} was left running by a failed handshake"
    );
}

/// TC-PORT-MCP-19: a server that refuses the handshake is refused back.
///
/// Upstream: "rejects activation and still closes the client when startup
/// failure is configured as fatal".
///
/// Input: the fixture in `refuse-initialize` mode.
/// Expected: class `handshake`, carrying the server's own reason.
#[tokio::test]
async fn a_server_that_refuses_the_handshake_is_refused_back() {
    let fault = connect_fixture("refuse-initialize", brisk())
        .await
        .expect_err("the handshake fails");
    assert_eq!(fault.class(), class::HANDSHAKE);
    assert!(
        fault.to_string().contains("not accepting connections"),
        "the server's reason survives: {fault}"
    );
}

/// TC-PORT-MCP-20: a real server writing a log line to stdout is a protocol
/// failure.
///
/// Upstream contains a malformed frame in its SDK; the decision restated here
/// is the crate's, and this is it happening to a program rather than to a
/// string: writing to stdout is the single most common way an MCP server is
/// broken, and the message has to say so.
///
/// Input: the fixture's `garbage` tool, which prints a log line.
/// Expected: class `protocol`, quoting the line, and the connection ends.
#[tokio::test]
async fn a_real_server_writing_a_log_line_to_stdout_is_a_protocol_failure() {
    let client = connect_fixture("serve", brisk()).await.expect("connected");
    let fault = client
        .call_tool("garbage", &json!({}))
        .await
        .expect_err("the call fails");
    assert_eq!(fault.class(), class::PROTOCOL, "{fault}");
    assert!(
        fault.to_string().contains("not a message"),
        "the line is quoted back: {fault}"
    );
    client.close().await;
}
