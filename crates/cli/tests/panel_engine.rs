//! The panel's protocol seam, against the binary this workspace builds.
//!
//! Every unit case in `web/deepseek/tests` plays the engine, which means every
//! one of them agrees with the panel by construction: a fold that expects
//! `call_id` and a fake peer written from the same misreading pass together.
//! Only a real engine can refuse that.
//!
//! So this case starts `tetanus serve` on a real socket, hands the address to
//! `web/deepseek/tests/engine.e2e.spec.ts`, and that suite dials it, runs a
//! turn that really executes a shell command, folds the journal the engine
//! actually wrote, and asserts on the rows a reader would see.
//!
//! # Why it lives in `crates/cli`
//!
//! Because it needs the binary, and `env!("CARGO_BIN_EXE_tetanus")` resolves
//! only in this crate's tests. That is also the reason it is not in
//! `crates/host` beside `panel_port.rs`: the panel's structure is one claim
//! and the panel's protocol is another, and only the second one needs a
//! process.
//!
//! # Missing Node
//!
//! The same rule as `panel_port.rs`, and for the same reason: a skip on a
//! developer's machine keeps the "no runtime to install" promise, and a skip
//! in CI is the absence of protection wearing protection's clothes.

use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The panel's directory, from this crate rather than the working directory.
fn panel() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/deepseek")
}

fn hosted() -> bool {
    std::env::var_os("CI").is_some()
}

/// What the toolchain can do here, or why it cannot.
fn missing() -> Option<String> {
    let ran = |program: &str| {
        Command::new(program)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    };
    if !ran("node") {
        return Some("no `node` on PATH".into());
    }
    if !ran("pnpm") {
        return Some("no `pnpm` on PATH".into());
    }
    if !panel().join("node_modules").is_dir() {
        return Some("web/deepseek/node_modules is absent; run `pnpm install` there".into());
    }
    None
}

/// A port nothing else on this machine is using.
///
/// Bound and released rather than guessed: two lanes share this box, and a
/// hard-coded port is a case that fails for a reason that has nothing to do
/// with the panel.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let port = listener.local_addr().expect("an address").port();
    drop(listener);
    port
}

/// A server, killed when this value is dropped.
struct Serving {
    child: Child,
    port: u16,
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Serving {
    /// Start the server and wait until the socket answers.
    fn start(sessions: &std::path::Path) -> Result<Serving, String> {
        let port = free_port();
        let mut child = Command::new(env!("CARGO_BIN_EXE_tetanus"))
            .args([
                "serve",
                "--listen",
                &format!("127.0.0.1:{port}"),
                "--dir",
                &sessions.display().to_string(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("the binary would not start: {err}"))?;

        // Wait for the port rather than for a banner: the banner is a
        // presentation decision and the port is the fact this needs.
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Ok(Serving { child, port });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let said = child
            .stderr
            .take()
            .map(|err| {
                BufReader::new(err)
                    .lines()
                    .map_while(Result::ok)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let _ = child.wait();
        Err(format!("the server never bound 127.0.0.1:{port}\n{said}"))
    }
}

/// TC-PANEL-ENGINE-1: the panel folds a turn this binary actually ran.
///
/// Expected: `pnpm run test:engine` exits 0 against a live server. The suite's
/// own assertions are the detail - a user row, a settled assistant row, a tool
/// card carrying what the command printed, and every turn closed - and they
/// are stated there rather than duplicated here, because they are claims about
/// the panel and this file is the harness.
#[test]
fn the_panel_folds_a_turn_this_binary_ran() {
    if let Some(why) = missing() {
        assert!(
            !hosted(),
            "TC-PANEL-ENGINE-1: {why}. In CI that is a failure, not a skip: \
             this is the only case where the panel meets the engine rather \
             than a peer written from the same assumptions, so skipping it \
             leaves the whole wire untested."
        );
        eprintln!(
            "TC-PANEL-ENGINE-1: {why}, so the panel was NOT run against the \
             engine. This is a skip only because this is not CI."
        );
        return;
    }

    let sessions = tempfile::tempdir().expect("a temp dir");
    let serving = match Serving::start(sessions.path()) {
        Ok(serving) => serving,
        Err(why) => panic!("TC-PANEL-ENGINE-1: {why}"),
    };

    let ran = Command::new("pnpm")
        .args(["run", "test:engine"])
        .current_dir(panel())
        .env(
            "TETANUS_PANEL_CARRIER",
            format!("ws://127.0.0.1:{}/api/ws", serving.port),
        )
        .output()
        .expect("pnpm runs");

    assert!(
        ran.status.success(),
        "TC-PANEL-ENGINE-1: the panel does not agree with the engine:\n{}\n{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr),
    );

    // A suite that skipped itself reports success, which is the one way this
    // case can pass while proving nothing.
    let said = String::from_utf8_lossy(&ran.stdout);
    assert!(
        !said.contains("skipped"),
        "TC-PANEL-ENGINE-1: the suite skipped rather than ran - the address \
         did not reach it:\n{said}"
    );
}
