//! Test Design Specification: `tetanus serve`, the subcommand that hands the
//! binary over to the protocol.
//!
//! Features tested: that a peer's frame is answered on stdout; that stdout
//! carries frames and nothing else, which is the property the whole
//! subcommand exists to hold; that stderr carries the page and nothing else,
//! pipe or no pipe; that a peer who says nothing still gets a clean exit; that
//! `--dir` reaches the banner; and, for `--listen`, that the banner names the
//! port that was actually bound, that a peer dialling it completes the
//! handshake, and that an address which cannot be bound announces no server.
//!
//! Features NOT tested here: the carrier's own behaviour - framing,
//! concurrency, push ordering - which is `tetanus-rpc`'s, and the wording of
//! the banner, which is asserted against a buffer in `render::serve`.
//!
//! Environmental needs: a loopback socket on a port the operating system
//! chooses, for the two `--listen` cases. No case needs a key, an outbound
//! network or a terminal.
//!
//! Procedure: every case spawns the binary with all three streams piped,
//! writes whole frames, reads whole lines, and closes stdin to end the run.
//! A peer that pipelines a second call before the first is answered has two
//! calls in flight (contract §4.1), so each case waits for the answer it asked
//! for before it writes again.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, Output, Stdio};

const HELLO: &str = r#"{"jsonrpc":"2.0","id":1,"method":"rpc.hello","params":{"protocol_version":"1.0","client":{"name":"probe","version":"0.1.0"}}}"#;

/// A server with all three streams piped, in a directory of its own.
fn serve(dir: &Path, args: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_tetanus"))
        .current_dir(dir)
        .arg("serve")
        .args(args)
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color")
        .env("COLUMNS", "100")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs")
}

/// Write one frame, then read the one line that answers it.
fn exchange(server: &mut Child, reader: &mut BufReader<ChildStdout>, frame: &str) -> String {
    let stdin = server.stdin.as_mut().expect("stdin is piped");
    writeln!(stdin, "{frame}").expect("the peer writes");
    stdin.flush().expect("the peer flushes");
    let mut line = String::new();
    reader.read_line(&mut line).expect("the server answers");
    line
}

/// Close stdin and collect what the run wrote.
fn hang_up(mut server: Child, reader: BufReader<ChildStdout>) -> (String, Output) {
    drop(server.stdin.take());
    let mut rest = String::new();
    let mut reader = reader;
    while reader.read_line(&mut rest).expect("stdout reads") > 0 {}
    let out = server.wait_with_output().expect("the server exits");
    (rest, out)
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("utf-8")
}

/// TC-CLI-SERVE-1: the handshake.
/// Expected: `rpc.hello` is answered on stdout with the same id, the server's
/// own protocol version, and the capabilities it serves. Nothing else works
/// until this call does, so it is the first thing a peer author checks.
#[test]
fn the_handshake_is_answered_on_stdout() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut server = serve(dir.path(), &[]);
    let mut reader = BufReader::new(server.stdout.take().expect("stdout is piped"));

    let answered = exchange(&mut server, &mut reader, HELLO);
    let frame: serde_json::Value = serde_json::from_str(&answered).expect("a JSON frame");

    assert_eq!(frame["jsonrpc"], "2.0", "{answered}");
    assert_eq!(frame["id"], 1, "{answered}");
    assert_eq!(frame["result"]["protocol_version"], "1.0", "{answered}");
    assert!(frame["result"]["capabilities"].is_array(), "{answered}");

    let (_, out) = hang_up(server, reader);
    assert!(out.status.success(), "{}", stderr(&out));
}

/// TC-CLI-SERVE-2: what stdout carries.
/// Expected: every line on stdout parses as a JSON-RPC frame, and no word of
/// the banner reaches it. This is the property the subcommand exists to hold:
/// one human line on that stream is a parse error in the peer, and a peer
/// reports it as "the server is broken".
#[test]
fn stdout_carries_frames_and_nothing_else() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut server = serve(dir.path(), &[]);
    let mut reader = BufReader::new(server.stdout.take().expect("stdout is piped"));

    let mut page = exchange(&mut server, &mut reader, HELLO);
    page.push_str(&exchange(
        &mut server,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":2,"method":"session.list","params":{}}"#,
    ));
    let (rest, out) = hang_up(server, reader);
    page.push_str(&rest);

    for line in page.lines() {
        let frame: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|_| panic!("not a frame: {line}"));
        assert_eq!(frame["jsonrpc"], "2.0", "{line}");
    }
    for leaked in ["tetanus serving", "sessions ", "note:", "Ctrl-D"] {
        assert!(!page.contains(leaked), "`{leaked}` reached stdout:\n{page}");
    }
    assert!(out.status.success(), "{}", stderr(&out));
}

/// TC-CLI-SERVE-3: what a person reads, and where.
/// Expected: stderr is the banner and the closing line, and nothing else, even
/// though stderr is a pipe. They are content, not animation, so the rule the
/// progress line follows applies: a repainted frame is held back from a pipe,
/// a sentence is not. The comparison is by equality rather than by `contains`
/// because for this subcommand stderr is the whole of what a user sees: a line
/// nobody chose to show them is a defect on the only stream they read, and
/// this is the case that finds it.
#[test]
fn stderr_is_the_page_and_nothing_else() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut server = serve(dir.path(), &[]);
    let mut reader = BufReader::new(server.stdout.take().expect("stdout is piped"));

    exchange(&mut server, &mut reader, HELLO);
    let (_, out) = hang_up(server, reader);

    assert_eq!(
        stderr(&out),
        "\ntetanus serving on stdio\n\
         sessions  sessions\n\
         protocol  1.0\n\
         note: end with Ctrl-D\n\
         note: the peer closed stdin, so the server stopped\n"
    );
}

/// TC-CLI-SERVE-4: a peer that says nothing.
/// Expected: end of file is the ordinary end of a connection, so the server
/// exits 0 with an empty stdout. A wrapper that starts the binary and dies
/// before writing must not leave a failure in the log for someone to chase.
#[test]
fn a_peer_that_never_speaks_still_ends_cleanly() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut server = serve(dir.path(), &[]);
    let reader = BufReader::new(server.stdout.take().expect("stdout is piped"));

    let (rest, out) = hang_up(server, reader);

    assert_eq!(rest, "", "a silent peer was answered anyway");
    assert_eq!(out.stdout, b"", "a silent peer was answered anyway");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("the peer closed stdin"),
        "{}",
        stderr(&out)
    );
}

/// TC-CLI-SERVE-5: `--dir`.
/// Expected: the banner names the directory given, not the default. Where the
/// journals land is the one setting this subcommand takes, and the banner is
/// the only place a user reads it before the work goes there.
#[test]
fn the_banner_names_the_directory_that_was_asked_for() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut server = serve(dir.path(), &["--dir", "elsewhere/journals"]);
    let reader = BufReader::new(server.stdout.take().expect("stdout is piped"));

    let (_, out) = hang_up(server, reader);
    let said = stderr(&out);

    assert!(said.contains("sessions  elsewhere/journals"), "{said}");
}

/// A WebSocket server on a port the operating system chooses, read far enough
/// to learn which one.
///
/// The address has to come from the banner rather than from the test, because
/// asking for port 0 is the only way to run this suite on a machine that is
/// already using whatever port the test would otherwise have picked.
fn listening(dir: &Path) -> (Child, BufReader<ChildStderr>, String) {
    let mut server = serve(dir, &["--listen", "127.0.0.1:0"]);
    let mut page = BufReader::new(server.stderr.take().expect("stderr is piped"));
    let mut address = None;
    let mut line = String::new();
    while address.is_none() {
        line.clear();
        if page.read_line(&mut line).expect("stderr reads") == 0 {
            break;
        }
        address = line
            .strip_prefix("address ")
            .map(|said| said.trim().to_string());
    }
    match address {
        Some(address) => (server, page, address),
        // Ended rather than left running: a panic here would otherwise leave
        // a process holding a port for the rest of the suite.
        None => {
            let _ = server.kill();
            let _ = server.wait();
            panic!("the banner never named an address");
        }
    }
}

/// Dial the socket, send one frame, and return the first frame that comes
/// back.
fn over_websocket(address: &str, frame: &str) -> String {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(async {
            let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}"))
                .await
                .expect("the server accepts a handshake");
            socket
                .send(Message::text(frame))
                .await
                .expect("the peer writes");
            loop {
                let message = socket
                    .next()
                    .await
                    .expect("the server answers")
                    .expect("a frame");
                if let Message::Text(text) = message {
                    return text.to_string();
                }
            }
        })
}

/// Ask the process to stop the way its own banner said to, and collect the
/// run.
fn interrupt(server: Child, page: BufReader<ChildStderr>) -> (Output, String) {
    let killed = Command::new("kill")
        .args(["-INT", &server.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(killed.success(), "the interrupt was not delivered");
    let mut rest = String::new();
    let mut page = page;
    while page.read_line(&mut rest).expect("stderr reads") > 0 {}
    let out = server.wait_with_output().expect("the server exits");
    (out, rest)
}

/// TC-CLI-SERVE-6: the WebSocket carrier, end to end through the binary.
/// Expected: the banner names the port that was actually bound, a peer
/// dialling that port completes the handshake, and the interrupt the banner
/// named ends the process with status 0 and a closing line. Contract §4.7
/// says `tetanus serve` hosts both carriers; this is the case that says the
/// second one is reachable from the command line and not only from the crate.
#[test]
fn the_websocket_carrier_answers_on_the_port_it_bound() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (server, page, address) = listening(dir.path());

    assert!(
        address.starts_with("127.0.0.1:") && !address.ends_with(":0"),
        "the banner named `{address}`, which nobody can connect to"
    );

    let answered = over_websocket(&address, HELLO);
    let frame: serde_json::Value = serde_json::from_str(&answered).expect("a JSON frame");
    assert_eq!(frame["id"], 1, "{answered}");
    assert_eq!(frame["result"]["protocol_version"], "1.0", "{answered}");

    let (out, rest) = interrupt(server, page);
    assert!(out.status.success(), "{rest}");
    assert!(
        rest.contains("interrupted, so the server stopped"),
        "no closing line:\n{rest}"
    );
    assert_eq!(out.stdout, b"", "the WebSocket carrier wrote to stdout");
}

/// TC-CLI-SERVE-7: an address that cannot be bound.
/// Expected: no banner, the address in the message, and the exit status §4.5
/// gives `Io`. Announcing a server and then failing to start it is the one
/// outcome a supervisor cannot recover from, because its log says the server
/// came up.
#[test]
fn an_address_that_cannot_be_bound_never_announces_a_server() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = serve(dir.path(), &["--listen", "1.2.3.4:80"])
        .wait_with_output()
        .expect("the binary exits");
    let said = String::from_utf8(out.stderr).expect("utf-8");

    assert_eq!(out.status.code(), Some(1), "{said}");
    assert!(said.starts_with("error: 1.2.3.4:80: "), "{said}");
    assert!(!said.contains("tetanus serving"), "{said}");
}
