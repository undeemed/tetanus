//! Test Design Specification: the servers a document declares, and what a
//! harness does when one of them will not start.
//!
//! Feature under test: `tetanus_mcp::settings` - reading `mcp.servers.*`,
//! `mcp.reconnect.*` and the budgets out of the layered config, and connecting
//! what it found. Upstream's equivalent is its plugin configuration, read by
//! Schemastery at load (`mcp-client.spec.ts`, "Config schema ..." and "apply
//! fails loud at load on a misconfigured reconnect").
//!
//! Approach: a config built key by key, and the real fixture server for the
//! cases that connect. A document is the input a deployment actually writes,
//! and the failures worth pinning are the ones a typo produces.
//!
//! What is not restated, and why. Upstream refuses an unknown key in its
//! reconnect block, because Schemastery knows every key; a tetanus document is
//! flat dotted keys with no per-namespace schema yet, which is the gap
//! `docs/parity.md` already carries for `settings/*`. Its `serverName`
//! reservation per app root has no counterpart: the id is a document key, so
//! it is unique by construction.
//!
//! Environmental needs: the fixture binary for the two connecting cases. No
//! network, no key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::time::Duration;

use tetanus_config::{Config, Layer};
use tetanus_mcp::settings::{self, key};
use tetanus_mcp::tools::public_name;
use tetanus_turn::tools::{EchoTool, ToolRegistry};

fn document(pairs: &[(&str, serde_json::Value)]) -> Config {
    let mut config = Config::default();
    for (key, value) in pairs {
        config.set(key, value.clone(), Layer::File);
    }
    config
}

/// TC-PORT-MCP-34: a server is a name, a command, its arguments and its
/// environment.
///
/// Upstream: "Config schema accepts a valid serverName", and its stdio
/// transport configuration.
///
/// Input: a document declaring two servers, one of them switched off, with
/// arguments, an environment and a working directory.
/// Expected: both read, in name order, carrying exactly what was written -
/// and nothing else in the environment, which is the rule this transport runs
/// under.
#[test]
fn a_server_is_a_name_a_command_its_arguments_and_its_environment() {
    let settings = document(&[
        ("mcp.servers.files.command", serde_json::json!("mcp-files")),
        (
            "mcp.servers.files.args",
            serde_json::json!(["--root", "/srv"]),
        ),
        ("mcp.servers.files.env.RUST_LOG", serde_json::json!("warn")),
        ("mcp.servers.files.cwd", serde_json::json!("/srv")),
        ("mcp.servers.notes.command", serde_json::json!("mcp-notes")),
        ("mcp.servers.notes.enabled", serde_json::json!(false)),
    ]);

    let declared = settings::servers(&settings).expect("the document reads");
    let names: Vec<&str> = declared.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["files", "notes"]);

    let files = &declared[0];
    assert!(files.enabled);
    assert_eq!(files.command.program, "mcp-files");
    assert_eq!(files.command.args, vec!["--root", "/srv"]);
    assert_eq!(
        files.command.env.get("RUST_LOG").map(String::as_str),
        Some("warn")
    );
    assert_eq!(
        files.command.env.len(),
        1,
        "the child gets what the document listed and nothing else"
    );
    assert_eq!(
        files.command.cwd.as_deref(),
        Some(std::path::Path::new("/srv"))
    );
    assert!(!declared[1].enabled, "a server can be switched off");
}

/// TC-PORT-MCP-35: a server with no command is a mistake in the document.
///
/// Upstream: "Config schema rejects a missing serverName" - the same shape of
/// refusal, on the field that matters here.
///
/// Input: a server declared with arguments and no command, then one whose
/// arguments are not a list.
/// Expected: `BadValue` naming the key in both cases.
#[test]
fn a_server_with_no_command_is_a_mistake_in_the_document() {
    let no_command = document(&[("mcp.servers.files.args", serde_json::json!(["--root"]))]);
    let refused = settings::servers(&no_command).expect_err("no command");
    assert!(
        refused.to_string().contains("mcp.servers.files.command"),
        "{refused}"
    );

    let bad_args = document(&[
        ("mcp.servers.files.command", serde_json::json!("mcp-files")),
        ("mcp.servers.files.args", serde_json::json!("--root /srv")),
    ]);
    let refused = settings::servers(&bad_args).expect_err("args is a list");
    assert!(
        refused.to_string().contains("mcp.servers.files.args"),
        "{refused}"
    );
}

/// TC-PORT-MCP-36: the reconnect policy comes from the document, and a policy
/// that cannot be run is refused there.
///
/// Upstream: "Config schema materializes reconnect defaults and merges partial
/// overrides", "Config schema rejects an invalid reconnect block", "apply
/// fails loud at load on a misconfigured reconnect".
///
/// Input: a document setting some of the block; one setting an initial delay
/// past the ceiling; one setting a cap of zero.
/// Expected: the named values with the defaults under them, then a refusal for
/// each impossible policy, naming the block.
#[test]
fn the_reconnect_policy_comes_from_the_document_and_an_impossible_one_is_refused() {
    let partial = document(&[
        (key::INITIAL_DELAY, serde_json::json!(50)),
        (key::MAX_ATTEMPTS, serde_json::json!(3)),
    ]);
    let policy = settings::policy(&partial).expect("a runnable policy");
    assert_eq!(policy.initial_delay, Duration::from_millis(50));
    assert_eq!(policy.max_attempts, 3);
    assert_eq!(
        policy.max_delay,
        tetanus_mcp::ReconnectPolicy::default().max_delay,
        "what the document did not say keeps the default"
    );
    assert!(policy.enabled);

    let inverted = document(&[
        (key::INITIAL_DELAY, serde_json::json!(60_000)),
        (key::MAX_DELAY, serde_json::json!(1_000)),
    ]);
    let refused = settings::policy(&inverted).expect_err("a first wait past the ceiling");
    assert!(refused.to_string().contains("mcp.reconnect"), "{refused}");

    let no_attempts = document(&[(key::MAX_ATTEMPTS, serde_json::json!(0))]);
    assert!(
        settings::policy(&no_attempts).is_err(),
        "a cap of no attempts never reconnects"
    );
}

/// TC-PORT-MCP-37: the declared servers are started and their tools registered
/// beside the harness's own.
///
/// Upstream: "registers tools under server-qualified public names", through
/// the plugin configuration rather than through a composer.
///
/// Input: a document declaring the fixture server twice under two names, with
/// the environment that makes it behave, connected into a registry that
/// already holds `echo`.
/// Expected: both servers connect, their tools are registered under their own
/// namespaces, tetanus's own `echo` is untouched, and nothing is reported as
/// having failed.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_declared_servers_are_started_and_their_tools_registered() {
    let program = env!("CARGO_BIN_EXE_tetanus-mcp-fixture");
    let settings = document(&[
        ("mcp.servers.alpha.command", serde_json::json!(program)),
        (
            "mcp.servers.alpha.env.TETANUS_MCP_FIXTURE",
            serde_json::json!("serve"),
        ),
        ("mcp.servers.beta.command", serde_json::json!(program)),
        (
            "mcp.servers.beta.env.TETANUS_MCP_FIXTURE",
            serde_json::json!("serve"),
        ),
    ]);
    let declared = settings::servers(&settings).expect("read");

    let mut registry = ToolRegistry::new().with(std::sync::Arc::new(EchoTool));
    let (connected, refused) = settings::connect_all(
        &mut registry,
        &declared,
        settings::policy(&settings).expect("policy"),
        harness::brisk(),
    )
    .await;

    assert!(refused.is_empty(), "a server did not start: {refused:?}");
    assert_eq!(connected.len(), 2);
    let names: Vec<String> = registry.names().cloned().collect();
    assert!(names.contains(&"echo".to_string()), "{names:?}");
    assert!(names.contains(&public_name("alpha", "echo")), "{names:?}");
    assert!(names.contains(&public_name("beta", "echo")), "{names:?}");

    for server in &connected {
        server.supervisor.shutdown().await;
    }
}

/// TC-PORT-MCP-38: a server that will not start does not stop the harness.
///
/// Upstream: "logs error and registers no tools when connect fails" - the
/// non-fatal half of its startup policy, which is the half a laptop needs.
///
/// One bad line in a document must not be a harness nobody can use: the tools
/// are an addition, and a deployment finds out which server is broken by being
/// told, not by having nothing work.
///
/// Input: two servers, one of them a program that does not exist.
/// Expected: the working server's tools are registered, the broken one is
/// reported by name with a class, and the call returns rather than failing.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_server_that_will_not_start_does_not_stop_the_harness() {
    let settings = document(&[
        (
            "mcp.servers.good.command",
            serde_json::json!(env!("CARGO_BIN_EXE_tetanus-mcp-fixture")),
        ),
        (
            "mcp.servers.good.env.TETANUS_MCP_FIXTURE",
            serde_json::json!("serve"),
        ),
        (
            "mcp.servers.missing.command",
            serde_json::json!("/nonexistent/mcp-server-that-is-not-installed"),
        ),
    ]);
    let declared = settings::servers(&settings).expect("read");

    let mut registry = ToolRegistry::new().with(std::sync::Arc::new(EchoTool));
    let (connected, refused) = settings::connect_all(
        &mut registry,
        &declared,
        settings::policy(&settings).expect("policy"),
        harness::brisk(),
    )
    .await;

    assert_eq!(connected.len(), 1);
    assert_eq!(connected[0].name, "good");
    assert!(registry
        .names()
        .any(|name| name == &public_name("good", "echo")));

    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].name, "missing");
    assert_eq!(
        refused[0].fault.class(),
        tetanus_mcp::fault::class::TRANSPORT,
        "{:?}",
        refused[0].fault
    );

    for server in &connected {
        server.supervisor.shutdown().await;
    }
}
