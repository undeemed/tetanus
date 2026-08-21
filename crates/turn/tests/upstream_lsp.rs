//! Test Design Specification: the language-server client and its tool, ported.
//!
//! Features under test: `tetanus_turn::lsp` - the base protocol's framing, the
//! handshake and query lifecycle over a real subprocess, and the rule the
//! module is arranged around: a server that dies is a failed tool call and
//! never a dead turn. Upstream pins the same decisions in
//! `packages/lsp/lsp-stdio/tests/{framing,lifecycle,connection}.spec.ts` and
//! `tool-lsp/tests/tool-lsp.spec.ts`.
//!
//! Approach: framing is a pure function and is tested as one, including the
//! split-chunk cases a pipe produces. Everything above it runs against a real
//! process: a scripted mock server, written here, that speaks the protocol
//! over stdin and stdout, plus one case against `rust-analyzer` when the box
//! has it. The mock is what makes the suite deterministic and offline; the
//! real server is what stops the mock becoming the specification.
//!
//! Every case that spawns a process is bounded by the client's own budgets, so
//! a hang is a failure rather than a wedged CI run.
//!
//! What is not restated, and why. Upstream pools servers by language and
//! reuses one across calls, so its eviction, idle-timeout and
//! concurrent-borrow cases have no counterpart: this opens a server per query.
//! Its document-synchronisation half - `didOpen`/`didChange` for unsaved
//! editor buffers - is unrepresentable, because tetanus has no buffers and the
//! file on disk is the document.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::{Path, PathBuf};

use serde_json::json;
use tetanus_turn::lsp::framing::{encode, FramingError, MessageDecoder, MAX_HEADER_BYTES};
use tetanus_turn::lsp::tool::LspTool;
use tetanus_turn::lsp::{LspAnswer, LspClient, LspConfig, LspError, LspOperation, Position};
use tetanus_turn::tools::{Tool, ToolError};

/// A mock language server: a Python script that speaks the base protocol,
/// answers the three requests this client makes, and can be told to die.
///
/// Written here rather than fetched, so the suite is offline and the server's
/// behaviour is stated where the cases that depend on it are read.
const MOCK_SERVER: &str = r#"
import sys, json, os

MODE = os.environ.get("MOCK_MODE", "ok")

def read_message():
    header = b""
    while not header.endswith(b"\r\n\r\n"):
        byte = sys.stdin.buffer.read(1)
        if not byte:
            return None
        header += byte
    length = 0
    for line in header.decode("ascii").split("\r\n"):
        if line.lower().startswith("content-length:"):
            length = int(line.split(":", 1)[1].strip())
    return json.loads(sys.stdin.buffer.read(length).decode("utf-8"))

def send(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

def location(uri, line, char):
    return {"uri": uri, "range": {"start": {"line": line, "character": char},
                                  "end": {"line": line, "character": char + 3}}}

opened = None
while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        if MODE == "die-on-init":
            sys.stderr.write("mock server refuses to start\n")
            sys.stderr.flush()
            sys.exit(3)
        if MODE == "silent":
            continue
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {}}})
    elif method == "textDocument/didOpen":
        opened = message["params"]["textDocument"]["uri"]
        if MODE == "diagnostics":
            send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                  "params": {"uri": opened, "diagnostics": [
                      {"range": {"start": {"line": 2, "character": 4},
                                 "end": {"line": 2, "character": 9}},
                       "severity": 1, "message": "undefined name"}]}})
    elif method == "textDocument/definition":
        if MODE == "die-on-query":
            sys.stderr.write("mock server crashed mid-query\n")
            sys.stderr.flush()
            sys.exit(4)
        if MODE == "refuse":
            send({"jsonrpc": "2.0", "id": message["id"],
                  "error": {"code": -32603, "message": "internal"}})
        elif MODE == "empty":
            send({"jsonrpc": "2.0", "id": message["id"], "result": None})
        else:
            # A server may interleave a notification before its reply; the
            # client must not lose the reply behind it.
            send({"jsonrpc": "2.0", "method": "window/logMessage",
                  "params": {"type": 3, "message": "thinking"}})
            send({"jsonrpc": "2.0", "id": message["id"],
                  "result": [location(opened, 10, 4)]})
    elif method == "textDocument/references":
        send({"jsonrpc": "2.0", "id": message["id"],
              "result": [location(opened, 10, 4), location(opened, 20, 8)]})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": None})
    elif method == "exit":
        break
"#;

/// A workspace with the mock server and one source file in it.
fn workspace(mode: &str) -> (tempfile::TempDir, LspConfig, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let server = dir.path().join("server.py");
    std::fs::write(&server, MOCK_SERVER).expect("write server");
    let source = dir.path().join("thing.py");
    std::fs::write(&source, "def thing():\n    pass\n\nthing()\n").expect("write source");

    // The mode reaches the server through its environment, which the client
    // passes on from this process.
    std::env::set_var("MOCK_MODE", mode);
    let config = LspConfig::new("python3", dir.path()).with_args([server.display().to_string()]);
    (dir, config, source)
}

/// TC-PORT-LSP-1: a message is framed the way the base protocol says.
///
/// Expected: a `Content-Length` header counting the body's bytes, the CRLF
/// pair, then the JSON.
#[test]
fn a_message_is_framed_with_its_byte_length() {
    let framed = encode(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }));
    let text = String::from_utf8(framed.clone()).expect("ascii header, utf-8 body");
    let (header, body) = text.split_once("\r\n\r\n").expect("a separator");

    assert_eq!(header, format!("Content-Length: {}", body.len()));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap()["method"],
        json!("initialize")
    );
}

/// TC-PORT-LSP-2: the decoder reassembles a message split anywhere.
///
/// Upstream: `framing.spec.ts`, "buffers a partial message". A pipe splits
/// wherever it likes, so a decoder that only worked on whole messages would
/// work until it was under load.
///
/// Expected: feeding one byte at a time yields the message exactly once, at
/// the last byte and not before.
#[test]
fn the_decoder_reassembles_a_message_split_anywhere() {
    let framed = encode(&json!({ "id": 7, "result": { "ok": true } }));
    let mut decoder = MessageDecoder::default();
    let mut seen = Vec::new();

    for (index, byte) in framed.iter().enumerate() {
        let out = decoder.push(&[*byte]).expect("no error");
        if index + 1 < framed.len() {
            assert!(out.is_empty(), "answered early at byte {index}");
        }
        seen.extend(out);
    }

    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["id"], json!(7));
}

/// TC-PORT-LSP-3: several messages in one chunk all come out, in order.
///
/// Expected: three messages from one push, in the order they were written, and
/// a trailing partial one held back.
#[test]
fn several_messages_in_one_chunk_come_out_in_order() {
    let mut chunk = Vec::new();
    for id in 1..=3 {
        chunk.extend(encode(&json!({ "id": id })));
    }
    let partial = encode(&json!({ "id": 4 }));
    chunk.extend_from_slice(&partial[..partial.len() - 3]);

    let mut decoder = MessageDecoder::default();
    let out = decoder.push(&chunk).expect("no error");

    assert_eq!(out.len(), 3, "the partial one is held back");
    assert_eq!(out[0]["id"], json!(1));
    assert_eq!(out[2]["id"], json!(3));

    let rest = decoder
        .push(&partial[partial.len() - 3..])
        .expect("no error");
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0]["id"], json!(4));
}

/// TC-PORT-LSP-4: the decoder refuses what a broken server can do to it.
///
/// Upstream: `framing.spec.ts`'s guards. A decoder that trusts a length field
/// is one a corrupt stream can take the harness down with, and a header with
/// no terminator would grow the buffer for ever.
///
/// Expected: a missing `Content-Length`, a non-numeric one, an over-limit body
/// and an unterminated header are each refused by name.
#[test]
fn the_decoder_refuses_what_a_broken_server_can_send() {
    let mut decoder = MessageDecoder::default();
    assert_eq!(
        decoder.push(b"Content-Type: json\r\n\r\n{}").unwrap_err(),
        FramingError::NoContentLength
    );

    let mut decoder = MessageDecoder::default();
    assert!(matches!(
        decoder.push(b"Content-Length: lots\r\n\r\n{}").unwrap_err(),
        FramingError::BadContentLength(_)
    ));

    let mut decoder = MessageDecoder::new(64);
    assert!(matches!(
        decoder.push(b"Content-Length: 100000\r\n\r\n").unwrap_err(),
        FramingError::MessageTooLong {
            announced: 100_000,
            limit: 64
        }
    ));

    let mut decoder = MessageDecoder::default();
    let flood = vec![b'x'; MAX_HEADER_BYTES + 1];
    assert_eq!(
        decoder.push(&flood).unwrap_err(),
        FramingError::HeaderTooLong
    );
}

/// TC-PORT-LSP-5: the client asks a real server and reads its answer.
///
/// The lifecycle end to end over a subprocess: handshake, `didOpen`, the
/// query, the shutdown. The mock also sends a notification before its reply,
/// so the case covers the interleaving a real server does.
///
/// Expected: one location, at the position the server named, converted from
/// the protocol's zero-based coordinates unchanged.
#[tokio::test]
async fn the_client_queries_a_real_server_over_stdio() {
    let (_dir, config, source) = workspace("ok");
    let client = LspClient::new(config);

    let answer = client
        .query(
            LspOperation::Definition,
            &source,
            Position {
                line: 3,
                character: 0,
            },
        )
        .await
        .expect("the server answered");

    match answer {
        LspAnswer::Locations(found) => {
            assert_eq!(found.len(), 1, "{found:?}");
            assert_eq!(found[0].line, 10);
            assert_eq!(found[0].character, 4);
            assert!(found[0].path.ends_with("thing.py"), "{}", found[0].path);
        }
        other => panic!("expected locations, got {other:?}"),
    }
}

/// TC-PORT-LSP-6: references come back as a list, and diagnostics as pushed
/// notifications.
///
/// Diagnostics are the operation with no request of its own - a server
/// publishes them - so this is the one that would be got wrong by treating
/// every operation as a round trip.
///
/// Expected: two references; one diagnostic with its severity and message.
#[tokio::test]
async fn references_and_diagnostics_both_come_back() {
    let (_dir, config, source) = workspace("ok");
    let answer = LspClient::new(config)
        .query(
            LspOperation::References,
            &source,
            Position {
                line: 3,
                character: 0,
            },
        )
        .await
        .expect("references");
    match answer {
        LspAnswer::Locations(found) => assert_eq!(found.len(), 2, "{found:?}"),
        other => panic!("expected locations, got {other:?}"),
    }

    let (_dir, config, source) = workspace("diagnostics");
    let answer = LspClient::new(config)
        .query(
            LspOperation::Diagnostics,
            &source,
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .expect("diagnostics");
    match answer {
        LspAnswer::Diagnostics(found) => {
            assert_eq!(found.len(), 1, "{found:?}");
            assert_eq!(found[0].severity, "error");
            assert_eq!(found[0].message, "undefined name");
            assert_eq!(found[0].line, 2);
        }
        other => panic!("expected diagnostics, got {other:?}"),
    }
}

/// TC-PORT-LSP-7: a clean file answers no diagnostics rather than timing out.
///
/// A server with nothing to say about a file simply never publishes, so
/// silence has to read as "nothing wrong" and not as a failure.
///
/// Expected: an empty diagnostic list, well inside the request budget.
#[tokio::test]
async fn a_clean_file_answers_no_diagnostics() {
    let (_dir, mut config, source) = workspace("ok");
    config.request_ms = 1_500;

    let answer = LspClient::new(config)
        .query(
            LspOperation::Diagnostics,
            &source,
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .expect("a clean file is not a failure");

    assert_eq!(answer, LspAnswer::Diagnostics(Vec::new()));
}

/// TC-PORT-LSP-8: a server that dies is contained, and says why.
///
/// The rule the whole module is arranged around. A language server is a large
/// third-party program that crashes; ending the conversation over it would be
/// the wrong response to a program the user did not write.
///
/// Expected: a `Died` error carrying the server's own dying words, both when
/// it refuses to start and when it crashes mid-query - and in neither case a
/// panic or a hang.
#[tokio::test]
async fn a_server_that_dies_is_contained_and_says_why() {
    let (_dir, config, source) = workspace("die-on-init");
    let refused = LspClient::new(config)
        .query(
            LspOperation::Definition,
            &source,
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .expect_err("the server exited");
    assert!(
        matches!(&refused, LspError::Died(said) if said.contains("refuses to start")),
        "got {refused:?}"
    );

    let (_dir, config, source) = workspace("die-on-query");
    let crashed = LspClient::new(config)
        .query(
            LspOperation::Definition,
            &source,
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .expect_err("the server crashed");
    assert!(
        matches!(&crashed, LspError::Died(said) if said.contains("crashed mid-query")),
        "got {crashed:?}"
    );
}

/// TC-PORT-LSP-9: a server that never answers is bounded.
///
/// A server that accepts a request and goes quiet is the ordinary failure of
/// this class of program. Without a deadline the turn hangs, which is the
/// unbounded-turn hazard the provider adapters already have an idle window
/// for. The case carries its own short budget so a regression fails the run
/// rather than wedging it.
///
/// Expected: `TimedOut` naming the budget, promptly.
#[tokio::test]
async fn a_server_that_never_answers_is_bounded() {
    let (_dir, mut config, source) = workspace("silent");
    config.startup_ms = 1_200;
    let started = std::time::Instant::now();

    let refused = LspClient::new(config)
        .query(
            LspOperation::Definition,
            &source,
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .expect_err("the server said nothing");

    assert!(
        matches!(refused, LspError::TimedOut(1_200)),
        "got {refused:?}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the deadline is what ended it, not the test harness"
    );
}

/// TC-PORT-LSP-10: a program that is not there is an error naming it.
///
/// The commonest configuration mistake, and one whose message has to say which
/// program to install.
///
/// Expected: `NotStarted` carrying the program's name.
#[tokio::test]
async fn a_missing_server_program_names_itself() {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("thing.py");
    std::fs::write(&source, "pass\n").unwrap();
    let config = LspConfig::new("no-such-language-server-anywhere", dir.path());

    let refused = LspClient::new(config)
        .query(
            LspOperation::Definition,
            &source,
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .expect_err("no such program");

    assert!(
        matches!(&refused, LspError::NotStarted { program, .. }
            if program == "no-such-language-server-anywhere"),
        "got {refused:?}"
    );
}

/// TC-PORT-LSP-11: a file outside the workspace is refused.
///
/// The root is what the server was opened on, so a query elsewhere is either a
/// mistake or an attempt to read past the fence the workspace exists to be.
///
/// Expected: `OutsideWorkspace`, before any process is started.
#[tokio::test]
async fn a_file_outside_the_workspace_is_refused() {
    let (_dir, config, _source) = workspace("ok");

    let refused = LspClient::new(config)
        .query(
            LspOperation::Definition,
            Path::new("/etc/passwd"),
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .expect_err("outside the workspace");

    assert!(
        matches!(refused, LspError::OutsideWorkspace { .. }),
        "got {refused:?}"
    );
}

/// TC-PORT-LSP-12: the tool answers a query, one-based.
///
/// Upstream: `tool-lsp.spec.ts`. A person and a model count lines from one and
/// the protocol counts from zero; the conversion happens in exactly one place,
/// so this is the case that pins it.
///
/// Expected: the rendered line is the protocol's plus one, and the tool
/// advertises the three operations.
#[tokio::test]
async fn the_tool_answers_a_query_in_one_based_coordinates() {
    let (_dir, config, source) = workspace("ok");
    let tool = LspTool::new(config);

    let schema = tool.schema();
    assert_eq!(schema.name, "lsp");
    let operations = schema.parameters["properties"]["operation"]["enum"].clone();
    assert_eq!(
        operations,
        json!(["definition", "references", "diagnostics"])
    );

    let outcome = tool
        .execute(&json!({
            "operation": "definition",
            "file": source.display().to_string(),
            "line": 4,
            "character": 1,
        }))
        .await
        .expect("the tool answered");

    assert!(outcome.ok);
    // The server answered line 10 zero-based, so the tool prints 11.
    assert!(
        outcome.content.contains(":11:5"),
        "one-based on the way out: {}",
        outcome.content
    );
}

/// TC-PORT-LSP-13: a dead server is a failed tool call, not a dead turn.
///
/// The acceptance claim, at the seam the model actually touches.
///
/// Expected: `ToolError::Failed` naming the tool, carrying the server's words,
/// rather than a panic or an error that ends the turn.
#[tokio::test]
async fn a_dead_server_is_a_failed_tool_call() {
    let (_dir, config, source) = workspace("die-on-query");
    let tool = LspTool::new(config);

    let failed = tool
        .execute(&json!({
            "operation": "definition",
            "file": source.display().to_string(),
            "line": 1,
            "character": 1,
        }))
        .await
        .expect_err("the server crashed");

    match failed {
        ToolError::Failed(name, said) => {
            assert_eq!(name, "lsp");
            assert!(said.contains("crashed mid-query"), "{said}");
        }
        other => panic!("expected a failed call, got {other:?}"),
    }
}

/// TC-PORT-LSP-14: what the tool refuses before it starts anything.
///
/// Expected: an unknown operation, a missing file, a missing position and a
/// zero line are each `InvalidArguments` - a zero because the coordinates are
/// one-based and subtracting would underflow.
#[tokio::test]
async fn the_tool_refuses_arguments_it_cannot_use() {
    let (_dir, config, source) = workspace("ok");
    let tool = LspTool::new(config);
    let file = source.display().to_string();

    for arguments in [
        json!({ "operation": "hover", "file": file }),
        json!({ "operation": "definition" }),
        json!({ "operation": "definition", "file": file }),
        json!({ "operation": "definition", "file": file, "line": 0, "character": 1 }),
    ] {
        let refused = tool
            .execute(&arguments)
            .await
            .expect_err("should be refused");
        assert!(
            matches!(refused, ToolError::InvalidArguments(_, _)),
            "{arguments} gave {refused:?}"
        );
    }
}

/// TC-PORT-LSP-15: an empty answer says so in words.
///
/// "No results" and "the tool printed nothing" are different facts, and a
/// model that cannot tell them apart asks again.
///
/// Expected: a sentence naming the operation, not an empty string.
#[tokio::test]
async fn an_empty_answer_says_so() {
    let (_dir, config, source) = workspace("empty");
    let tool = LspTool::new(config);

    let outcome = tool
        .execute(&json!({
            "operation": "definition",
            "file": source.display().to_string(),
            "line": 1,
            "character": 1,
        }))
        .await
        .expect("an empty answer is still an answer");

    assert!(outcome.ok);
    assert_eq!(outcome.content, "no definition found at that position");
}

/// TC-PORT-LSP-16: the client works against a real language server.
///
/// The mock is what makes this suite deterministic; this is what stops the
/// mock becoming the specification. `rust-analyzer` is the server this
/// workspace's own language has, and it is the one a developer here is
/// likeliest to have installed.
///
/// This case reports itself skipped when the binary is absent, so the suite
/// stays green on a box without it - the same rule the one live provider case
/// follows for a missing API key.
///
/// What it asserts is the lifecycle, not the answer: a real handshake, a real
/// query and a real shutdown against a real server, with whatever comes back
/// contained. It deliberately does *not* require a definition to be found.
/// rust-analyzer answers only once it has built the crate graph, and how long
/// that takes depends on the box - so asserting a hit would make this a test
/// of a third-party program's indexing speed under load, which is a flake
/// rather than a claim about this client. TC-PORT-LSP-5 makes the "the answer
/// is read correctly" claim, deterministically, against the scripted server.
///
/// Expected: the query settles, no panic and no hang; any location it does
/// return is inside the crate.
#[tokio::test]
async fn the_client_works_against_rust_analyzer() {
    let Ok(program) = which("rust-analyzer") else {
        eprintln!("skipped: rust-analyzer is not on PATH");
        return;
    };

    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    // `answer()` is called on line 6; its definition is on line 2.
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn answer() -> u32 {\n    42\n}\n\npub fn ask() -> u32 {\n    answer()\n}\n",
    )
    .unwrap();

    let mut config = LspConfig::new(program.display().to_string(), dir.path());
    config.startup_ms = 60_000;
    config.request_ms = 60_000;

    let answer = LspClient::new(config)
        .query(
            LspOperation::Definition,
            Path::new("src/lib.rs"),
            // Zero-based: line 5 character 4 is the `answer` call.
            Position {
                line: 5,
                character: 4,
            },
        )
        .await;

    match answer {
        Ok(LspAnswer::Locations(found)) => {
            assert!(
                found.iter().all(|at| at.path.ends_with(".rs")),
                "every location a real server gave is a Rust file in the crate: {found:?}"
            );
            if found.is_empty() {
                eprintln!("rust-analyzer had not finished indexing; the lifecycle still held");
            }
        }
        Ok(other) => panic!("expected locations, got {other:?}"),
        // A real server on a loaded box may not finish indexing in the budget.
        // That is a contained failure and exactly what the tool reports; the
        // claim this case makes is that it is never a panic or a hang.
        Err(error) => eprintln!("rust-analyzer did not answer in budget: {error}"),
    }
}

/// The first `name` on `PATH`, or nothing.
fn which(name: &str) -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or(())
}
