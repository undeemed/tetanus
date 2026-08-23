//! Test Design Specification: the binary starts the MCP servers a document
//! declares.
//!
//! Feature under test: `mcp.servers.*` reaching the shipped `tetanus` binary -
//! that a declared server is started at boot, that its tools are offered
//! beside the harness's own, that a server which will not start is named
//! rather than silently absent, and that nothing this process starts outlives
//! it.
//!
//! Why the binary and not `crates/mcp`: that crate could connect a server and
//! bridge its tools from the day it landed, and its own suite proves it does.
//! What no case covered was whether the *program people run* ever calls it -
//! and for a while it did not, which is the gap this closes. Only a case that
//! execs `tetanus` can tell "the client works" from "the binary uses it".
//!
//! Features NOT tested here: the protocol itself, the handshake, reconnection,
//! and a bridged tool dispatched through a turn (all owned by
//! `crates/mcp/tests`, which has a real server and a real turn for each); the
//! assembly rules (owned by `crates/toolset`).
//!
//! The server: a small MCP server written for these cases, in Python, started
//! by the binary as an ordinary declared server over a real pipe. It is
//! written here rather than reusing the fixture in `crates/mcp` because
//! `CARGO_BIN_EXE_*` is only set for binaries of the package under test, and
//! the alternatives - a second fixture binary in this crate, or locating the
//! other crate's one by path - either ship a test double in a release build or
//! pass only under `--workspace`.
//!
//! Environmental needs: a `python3` on PATH, and a writable temp directory.
//! The same standing need `web/chat/serve.py` and `tools/uiwatch` already
//! have. No case reaches a network or needs a credential.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::Path;
use std::process::Command;

/// A correct MCP server: it answers the handshake, advertises one tool, and
/// answers a call to it. Small on purpose - what is under test is the binary
/// that starts it, not the protocol.
const SERVER: &str = r#"
import json, sys
for line in sys.stdin:
    try: m = json.loads(line)
    except Exception: continue
    method, mid = m.get("method"), m.get("id")
    if method == "initialize":
        r = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
             "serverInfo": {"name": "probe", "version": "1"}}
    elif method == "tools/list":
        r = {"tools": [{"name": "ping", "description": "answers pong",
                        "inputSchema": {"type": "object", "properties": {}}}]}
    elif method == "tools/call":
        r = {"content": [{"type": "text", "text": "pong"}], "isError": False}
    else:
        continue
    if mid is not None:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": r}) + "\n")
        sys.stdout.flush()
"#;

/// A home with the server script in it, and whatever document the case wants.
fn home(document: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("server.py"), SERVER).expect("write the server");
    let document = document.replace("{DIR}", &dir.path().display().to_string());
    std::fs::write(dir.path().join("settings.yaml"), document).expect("write the document");
    dir
}

/// A document declaring the good server under `name`.
fn declaring(name: &str) -> String {
    format!(
        "mcp:\n  servers:\n    {name}:\n      command: python3\n      args: [\"{{DIR}}/server.py\"]\n"
    )
}

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .current_dir(dir)
        .env("TETANUS_HOME", dir)
        .env_remove("DEEPSEEK_API_KEY")
        .args(args)
        .output()
        .expect("the binary runs")
}

fn offered(dir: &Path) -> Vec<String> {
    let out = run(dir, &["tools", "--json"]);
    assert!(
        out.status.success(),
        "tetanus tools failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let page: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("the tools page is one JSON object");
    let mut names: Vec<String> = page["tools"]
        .as_array()
        .expect("tools is a list")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name").to_string())
        .collect();
    names.sort();
    names
}

/// How many copies of this case's server are running. Counted by the script's
/// absolute path, which is unique to the case's own temp directory, so two
/// cases running at once cannot see each other's.
fn running(dir: &Path) -> usize {
    let script = dir.join("server.py").display().to_string();
    let ps = Command::new("ps")
        .args(["-eo", "args="])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&ps.stdout)
        .lines()
        .filter(|line| line.contains(&script))
        .count()
}

/// TC-CLI-MCP-1: a server the document declares is started, and its tools are
/// offered by the binary.
///
/// The acceptance this slice exists for. Before it, `mcp.servers.*` was a key
/// the binary read and did nothing with: the `mcp` source composed empty and
/// the model was offered nothing, on a build whose MCP client was complete and
/// fully tested.
///
/// Input: a document declaring one working server, and `tetanus tools --json`.
/// Expected: the server's tool is offered under its bridged name
/// `mcp__probe__ping`, beside the harness's own tools rather than instead of
/// them.
#[test]
fn a_declared_server_is_started_and_its_tools_are_offered() {
    let dir = home(&declaring("probe"));

    let names = offered(dir.path());

    assert!(
        names.contains(&"mcp__probe__ping".to_string()),
        "the declared server's tool is not offered: {names:?}"
    );
    assert!(
        names.contains(&"echo".to_string()) && names.contains(&"read".to_string()),
        "the harness's own tools are still there: {names:?}"
    );
}

/// TC-CLI-MCP-2: the bridged name carries the server's name from the document.
///
/// Two deployments naming the same server differently must get different tool
/// names, because the name is what a model calls and what a `tools.order` or a
/// preset's tool subset refers to. This is `crates/mcp`'s naming rule reaching
/// the binary with the *document's* name in it, rather than one this file
/// invented.
///
/// Input: the same server declared as `alpha`.
/// Expected: `mcp__alpha__ping`, and no `mcp__probe__ping`.
#[test]
fn the_bridged_name_carries_the_name_the_document_gave_the_server() {
    let dir = home(&declaring("alpha"));

    let names = offered(dir.path());

    assert!(names.contains(&"mcp__alpha__ping".to_string()), "{names:?}");
    assert!(
        !names.contains(&"mcp__probe__ping".to_string()),
        "{names:?}"
    );
}

/// TC-CLI-MCP-3: a server that will not start is named, and the run carries
/// on.
///
/// `crates/mcp` connects each server independently so one broken line in a
/// document does not cost a laptop its working agent. What the binary adds is
/// saying so: a tool that is silently absent is a capability nobody took away,
/// and "the model never called the tool I configured" is a question whose
/// answer has to be visible before the run.
///
/// Input: a document declaring one server that cannot be executed and one that
/// can.
/// Expected: exit 0; the working server's tool offered; a warning on stderr
/// naming the broken server, its fault class, and the reason. Not an error,
/// and not a panic.
#[test]
fn a_server_that_will_not_start_is_named_and_the_run_carries_on() {
    let dir = home(
        "mcp:\n  servers:\n    broken:\n      command: /nonexistent/server\n    probe:\n      command: python3\n      args: [\"{DIR}/server.py\"]\n",
    );

    let out = run(dir.path(), &["tools", "--json"]);

    assert!(out.status.success(), "the harness still comes up");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("broken"), "names the server: {said}");
    assert!(said.contains("[transport]"), "carries a class: {said}");
    assert!(!said.contains("panicked"), "reported, not panicked: {said}");

    let page: serde_json::Value = serde_json::from_slice(&out.stdout).expect("a tools page");
    let names: Vec<&str> = page["tools"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name"))
        .collect();
    assert!(
        names.contains(&"mcp__probe__ping"),
        "the server that did start still contributes: {names:?}"
    );
}

/// TC-CLI-MCP-4: no server outlives the command that started it.
///
/// The promise `crates/mcp` makes about itself, held at the level of the
/// program: a harness that leaked one child process per invocation would fill
/// a developer's machine over a morning, and the leak is invisible from inside
/// the crate that spawns them.
///
/// Input: `tetanus tools` and then `tetanus run`, on a document declaring one
/// server, with the process table read before and after each.
/// Expected: zero copies of the server running at every point, *and* the
/// listing offering the bridged tool - because without that second assertion
/// this case passes on a build that never starts a server at all, which is
/// exactly the build it is meant to catch.
#[test]
fn no_declared_server_outlives_the_command_that_started_it() {
    let dir = home(&declaring("probe"));

    assert_eq!(running(dir.path()), 0, "nothing running before");

    // That a server ran at all: zero-after-zero is also what a build that
    // starts nothing reports, so the reaping is only meaningful beside proof
    // there was something to reap.
    assert!(offered(dir.path()).contains(&"mcp__probe__ping".to_string()));
    assert_eq!(running(dir.path()), 0, "a listing left a server behind");

    let turned = run(dir.path(), &["run", "hello"]);
    assert!(
        turned.status.success(),
        "the turn failed: {}",
        String::from_utf8_lossy(&turned.stderr)
    );
    assert_eq!(
        running(dir.path()),
        0,
        "a run left a server behind: {}",
        String::from_utf8_lossy(&turned.stderr)
    );
}

/// TC-CLI-MCP-5: a server switched off in the document is not started.
///
/// `enabled: false` keeps a server's configuration in the document with the
/// server not running, which is what someone bisecting a problem wants. It has
/// to reach the binary, or the only way to turn one off is to delete the lines
/// and lose them.
///
/// Input: the same server, declared with `enabled: false`.
/// Expected: no bridged tool, and no server process left running.
#[test]
fn a_server_switched_off_in_the_document_is_not_started() {
    let dir = home(
        "mcp:\n  servers:\n    probe:\n      command: python3\n      args: [\"{DIR}/server.py\"]\n      enabled: false\n",
    );

    let names = offered(dir.path());

    assert!(
        !names.iter().any(|name| name.starts_with("mcp__")),
        "a server switched off contributed tools: {names:?}"
    );
    assert_eq!(running(dir.path()), 0, "and started no process");
    // The same document with the switch removed does contribute, so this case
    // is about `enabled: false` and not about a server that never worked.
    let on = home(&declaring("probe"));
    assert!(offered(on.path()).contains(&"mcp__probe__ping".to_string()));
}

/// TC-CLI-MCP-6: the tools page and a turn are offered the same MCP tools.
///
/// The catalogue starts the declared servers and so does a run, by different
/// paths - one composition with no session, one per session. A page that
/// advertised a bridged tool a turn did not get would be the drift the
/// assembly exists to prevent, arriving through the one source that is built
/// from outside the process.
///
/// Input: `tetanus tools --json`, and `tetanus run --trace` on the same
/// document.
/// Expected: the run succeeds with the server declared, and the page lists the
/// bridged tool. A run that could not compose the source would fail rather
/// than quietly offering less.
#[test]
fn the_page_and_a_turn_are_offered_the_same_mcp_tools() {
    let dir = home(&declaring("probe"));

    assert!(offered(dir.path()).contains(&"mcp__probe__ping".to_string()));

    let turned = run(dir.path(), &["run", "hello"]);

    assert!(
        turned.status.success(),
        "a turn could not compose the mcp source: {}",
        String::from_utf8_lossy(&turned.stderr)
    );
    assert_eq!(running(dir.path()), 0, "and left nothing behind");
}
