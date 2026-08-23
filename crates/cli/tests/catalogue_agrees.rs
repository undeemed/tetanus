//! Test Design Specification: every surface of one build reports the same
//! toolbox.
//!
//! Feature under test: that `tetanus tools`, `tetanus info`, `catalog.tools`
//! over stdio, `catalog.tools` over WebSocket, and `catalog.tools` through the
//! frontend's `/api/` bridge all answer with the same tools, on the same
//! binary, in one run.
//!
//! Why one case and not five: five cases that each pass in isolation are
//! exactly how this defect survived. `tetanus serve` answered `catalog.tools`
//! correctly and had a case saying so; `tetanus serve --frontend` built its
//! engine from `booted` alone and answered with the engine's offline minimum -
//! one tool, `echo`, on a build offering twenty-six - and had no case at all,
//! because the surface that was wrong was the one nobody had written a
//! catalogue case for. A case per surface cannot catch a surface that is
//! missing. A case that collects every surface and compares them to each other
//! fails the moment one of them disagrees, including a surface added later,
//! because the comparison is between them rather than against a number written
//! here.
//!
//! Deliberately no expected count: this asserts *agreement*, not twenty-six. A
//! build that composes a different set is free to; a build whose surfaces
//! disagree is not. Pinning the number here would make every tool crate that
//! lands edit this file, which is how an assertion becomes a formality.
//!
//! Features NOT tested here: what the tools are (owned by `toolset.rs` and by
//! each tool crate), the carriers themselves (owned by `serve.rs`), and the
//! frontend's own routes (owned by `crates/host`).
//!
//! Environmental needs: a writable temp directory and a loopback socket. No
//! case reaches a network or needs a credential.
//!
//! Pass criteria: every surface names the same set of tools.
//! Fail criteria: any two disagree, or a surface cannot be asked.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, Command, Stdio};

const HELLO: &str = r#"{"jsonrpc":"2.0","id":1,"method":"rpc.hello","params":{"protocol_version":"1.0","client":{"name":"probe","version":"0.1.0"}}}"#;
const TOKEN: &str = "a-token-this-case-states";
const CATALOG: &str = r#"{"jsonrpc":"2.0","id":2,"method":"catalog.tools","params":{}}"#;

/// The tool names in a `catalog.tools` result, from a whole JSON-RPC response.
fn names_in(response: &str) -> BTreeSet<String> {
    let message: serde_json::Value =
        serde_json::from_str(response).unwrap_or_else(|err| panic!("not JSON: {err}: {response}"));
    let result = message
        .get("result")
        .unwrap_or_else(|| panic!("no result in {response}"));
    result["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("no tools in {response}"))
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name").to_string())
        .collect()
}

fn binary(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tetanus"));
    cmd.current_dir(dir)
        .env("TETANUS_HOME", dir)
        .env_remove("DEEPSEEK_API_KEY")
        .args(args);
    cmd
}

/// What `tetanus tools --json` lists.
fn from_the_page(dir: &Path) -> BTreeSet<String> {
    let out = binary(dir, &["tools", "--json"])
        .output()
        .expect("the binary runs");
    assert!(
        out.status.success(),
        "tetanus tools failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let page: serde_json::Value = serde_json::from_slice(&out.stdout).expect("a JSON page");
    page["tools"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a name").to_string())
        .collect()
}

/// The count `tetanus info` prints, which is a second reader of the same
/// assembly and has disagreed with the page before.
fn from_info(dir: &Path) -> usize {
    let out = binary(dir, &["info"]).output().expect("the binary runs");
    assert!(out.status.success(), "tetanus info failed");
    let said = String::from_utf8_lossy(&out.stdout);
    let line = said
        .lines()
        .find(|line| line.trim_start().starts_with("tools"))
        .unwrap_or_else(|| panic!("info printed no tools row: {said}"));
    line.split_whitespace()
        .find_map(|word| word.parse::<usize>().ok())
        .unwrap_or_else(|| panic!("no count in {line:?}"))
}

/// `catalog.tools` over the stdio carrier, handshake first and answered before
/// the next frame is written - the two cannot be pipelined, because the
/// contract refuses every call before `rpc.hello`.
fn over_stdio(dir: &Path) -> BTreeSet<String> {
    let mut server = binary(dir, &["serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the server starts");
    let mut input = server.stdin.take().expect("stdin is piped");
    let mut output = BufReader::new(server.stdout.take().expect("stdout is piped"));

    let mut answered = |frame: &str| {
        writeln!(input, "{frame}").expect("the peer writes");
        input.flush().expect("flushed");
        let mut line = String::new();
        assert!(output.read_line(&mut line).expect("reads") > 0, "no answer");
        line
    };
    answered(HELLO);
    let catalogue = answered(CATALOG);

    drop(input);
    let _ = server.wait();
    names_in(&catalogue)
}

/// A server on a port the operating system chose, read far enough to learn
/// which one. The address comes from the banner for `serve.rs`'s reason: port
/// 0 is the only way to run this on a machine already using a fixed one.
fn listening(dir: &Path, args: &[&str]) -> (Child, BufReader<ChildStderr>, String) {
    let mut server = binary(dir, args)
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
        // The socket banner names a bare `host:port`; the frontend's names a
        // URL, with a path and possibly a token on it. Only the authority is
        // wanted, so it is cut out rather than trimmed off - trimming left the
        // token query on the end and dialled a path that does not exist.
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

/// `catalog.tools` over a WebSocket carrier, at whatever path it is mounted
/// on: the bare `serve` puts it at the root and the frontend at `/api/ws`.
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
                // The handshake's answer is read and dropped; the catalogue's
                // is the one this returns. Sequential on purpose: every call
                // before `rpc.hello` is refused, so pipelining the two races
                // the refusal.
                if frame == CATALOG {
                    return names_in(&answer);
                }
            }
            unreachable!("the catalogue frame is always sent")
        })
}

/// TC-CLI-CAT-12: every surface of one build reports the same toolbox.
///
/// The defect this pins: on a twenty-six-tool build, `tetanus tools` said
/// twenty-six and `tetanus serve --frontend` said one, because the served
/// engine's tools were composed at one call site instead of by a function
/// every serving surface goes through. Every client behind the frontend - the
/// browser panel, anything posting to `/api/` - saw a near-empty toolbox.
///
/// Input: one binary, one settings document, asked five ways in one case.
/// Expected: the page, `info`'s count, the stdio carrier, the WebSocket
/// carrier and the frontend's bridge all name the same tools. The assertion is
/// between the surfaces, so a sixth added later that gets this wrong fails
/// here without this file being edited to know about it.
#[test]
fn every_surface_of_one_build_reports_the_same_toolbox() {
    let home = tempfile::tempdir().expect("temp dir");
    let frontend = home.path().join("frontend");
    std::fs::create_dir_all(&frontend).expect("a frontend directory");
    std::fs::write(frontend.join("index.html"), "<html></html>").expect("an index");

    let page = from_the_page(home.path());
    assert!(
        !page.is_empty(),
        "a build with no tools proves nothing here"
    );

    let stdio = over_stdio(home.path());

    let (mut socket_server, _page, address) =
        listening(home.path(), &["serve", "--listen", "127.0.0.1:0"]);
    let websocket = over_websocket(&address, "");
    let _ = socket_server.kill();
    let _ = socket_server.wait();

    let (mut web_server, _banner, web_address) = listening(
        home.path(),
        &[
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--frontend",
            frontend.to_str().expect("utf-8"),
            // A stated token, because the frontend authenticates its carrier
            // and it rides in the reader's own URL.
            "--token",
            TOKEN,
        ],
    );
    // The frontend puts the carrier on `/api/ws`, which is upstream's
    // arrangement; the bare socket serves it at the root.
    let frontend_names = over_websocket(&web_address, &format!("/api/ws?token={TOKEN}"));
    let _ = web_server.kill();
    let _ = web_server.wait();

    assert_eq!(page, stdio, "the tools page and the stdio carrier disagree");
    assert_eq!(
        page, websocket,
        "the tools page and the WebSocket carrier disagree"
    );
    assert_eq!(
        page, frontend_names,
        "the tools page and the frontend's carrier disagree - this is the shape of the defect \
         TC-CLI-CAT-12 exists for: a serving surface that composed its own engine"
    );
    assert_eq!(
        from_info(home.path()),
        page.len(),
        "`tetanus info` counts a different number of tools than `tetanus tools` lists"
    );
}
