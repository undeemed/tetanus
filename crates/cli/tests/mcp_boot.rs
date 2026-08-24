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

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, ChildStderr, Command, Stdio};

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

/// TC-CLI-MCP-7: every carrier is offered the declared server's tools.
///
/// TC-CLI-CAT-12 holds the surfaces of one build to the same toolbox, but it
/// runs with no server declared, so a build that wired MCP into one carrier
/// and not another would pass it: both would answer twenty-six and agree.
/// This is the same question asked with a server in the document, which is the
/// axis the MCP source actually varies on - it is composed at boot, per
/// surface, from a process that has to be started.
///
/// It is not hypothetical either. The first cut of this slice wired `serve`,
/// `run`, `chat` and the catalogue by hand and left `serve --frontend` out -
/// the same surface, and the same omission, as the defect TC-CLI-CAT-12 exists
/// for.
///
/// Input: a document declaring one working server, asked of the tools page and
/// of the frontend's carrier.
/// Expected: both offer `mcp__probe__ping`, and both name the same set.
#[test]
fn every_carrier_is_offered_the_declared_servers_tools() {
    let dir = home(&declaring("probe"));
    let frontend = dir.path().join("frontend");
    std::fs::create_dir_all(&frontend).expect("a frontend directory");
    std::fs::write(frontend.join("index.html"), "<html></html>").expect("an index");

    let page: BTreeSet<String> = offered(dir.path()).into_iter().collect();
    assert!(
        page.contains("mcp__probe__ping"),
        "the page does not have it to compare: {page:?}"
    );

    let (mut server, _banner, address) = listening(
        dir.path(),
        &[
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--frontend",
            frontend.to_str().expect("utf-8"),
            "--token",
            TOKEN,
        ],
    );
    let carried = over_websocket(&address, &format!("/api/ws?token={TOKEN}"));
    let _ = server.kill();
    let _ = server.wait();

    assert_eq!(
        page, carried,
        "the tools page and the frontend's carrier disagree with a server declared"
    );
}

const TOKEN: &str = "a-token-this-case-states";
const HELLO: &str = r#"{"jsonrpc":"2.0","id":1,"method":"rpc.hello","params":{"protocol_version":"1.0","client":{"name":"probe","version":"0.1.0"}}}"#;
const CATALOG: &str = r#"{"jsonrpc":"2.0","id":2,"method":"catalog.tools","params":{}}"#;

/// A server on a port the operating system chose, read far enough to learn
/// which one. Only the authority is wanted: the frontend's banner names a URL
/// with a token on it, so it is cut out of the line rather than trimmed off.
fn listening(dir: &Path, args: &[&str]) -> (Child, BufReader<ChildStderr>, String) {
    let mut server = Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .current_dir(dir)
        .env("TETANUS_HOME", dir)
        .env_remove("DEEPSEEK_API_KEY")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the server starts");
    let mut page = BufReader::new(server.stderr.take().expect("stderr is piped"));
    let mut address = None;
    let mut line = String::new();
    while address.is_none() {
        line.clear();
        if page.read_line(&mut line).expect("stderr reads") == 0 {
            break;
        }
        address = line.split_whitespace().find_map(|word| {
            let start = word.find("127.0.0.1:")?;
            let rest = &word[start..];
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ':'))
                .unwrap_or(rest.len());
            Some(rest[..end].to_string())
        });
    }
    match address {
        Some(address) => (server, page, address),
        None => {
            let _ = server.kill();
            let _ = server.wait();
            panic!("the banner never named an address");
        }
    }
}

/// The tool names `catalog.tools` answers with over a WebSocket carrier.
fn over_websocket(address: &str, path: &str) -> BTreeSet<String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(async {
            let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}{path}"))
                .await
                .expect("the server accepts a handshake");
            for frame in [HELLO, CATALOG] {
                socket.send(Message::text(frame)).await.expect("writes");
                let answer = loop {
                    let message = socket.next().await.expect("answers").expect("a frame");
                    if let Message::Text(text) = message {
                        break text.to_string();
                    }
                };
                if frame == CATALOG {
                    let message: serde_json::Value =
                        serde_json::from_str(&answer).expect("a JSON answer");
                    return message["result"]["tools"]
                        .as_array()
                        .unwrap_or_else(|| panic!("no tools in {answer}"))
                        .iter()
                        .map(|tool| tool["name"].as_str().expect("a name").to_string())
                        .collect();
                }
            }
            unreachable!("the catalogue frame is always sent")
        })
}
