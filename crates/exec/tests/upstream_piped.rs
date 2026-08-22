//! Test Design Specification: a child this process talks to.
//!
//! Feature under test: `tetanus_exec::piped` - the seam a protocol peer is
//! started through. Upstream's equivalent is the stdio transport in
//! `packages/mcp` and the piped half of
//! `packages/subprocess/subprocess-local`: a long conversation with a program
//! that stays up, where stdout is the wire and closing stdin is how it is told
//! to go home.
//!
//! Approach: real children over real pipes. The claim that matters here cannot
//! be asserted against a fake at all - it is about what the operating system
//! does to a process group when the leader is killed, and a peer that starts
//! helpers of its own is the case that distinguishes this seam from the
//! `child.kill()` each consumer would otherwise write.
//!
//! What is not asserted here, and why. Framing is the consumer's: this seam
//! hands over two pipes and never reads them, so there is no line protocol to
//! test. `crates/mcp`'s own suite covers the MCP framing over this seam, which
//! is the arrangement under test as much as anything here.
//!
//! Environmental needs: a POSIX shell and a writable temp directory. No case
//! reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(unix)]

use std::time::Duration;

use tetanus_exec::piped::{Diagnostics, PipedCommand, PipedExit};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// TC-PORT-PROC-22: a peer is a conversation, not a result.
///
/// Upstream's stdio transport exists because `runCommand` is the wrong shape
/// for a server: collecting a child's output means waiting for it to exit, and
/// a protocol peer never does. This is the shape it needs instead - write,
/// read the answer, and the peer is still there.
///
/// Input: a shell reading lines and answering each one, written to twice.
/// Expected: each answer comes back before the next line is written, and the
/// peer's process id is reported for a caller that has to prove it left.
#[tokio::test]
async fn a_peer_is_a_conversation_and_not_a_result() {
    let mut child = peer("while read -r line; do echo \"you said: $line\"; done")
        .spawn()
        .expect("the peer started");
    assert!(
        child.pid().is_some(),
        "a caller needs the pid to prove it left"
    );

    let mut input = child.stdin().expect("its input");
    let mut output = BufReader::new(child.stdout().expect("its output")).lines();

    input.write_all(b"first\n").await.expect("written");
    input.flush().await.expect("flushed");
    assert_eq!(
        next(&mut output).await,
        Some("you said: first".to_string()),
        "the answer must come back while the peer is still running"
    );

    input.write_all(b"second\n").await.expect("written");
    input.flush().await.expect("flushed");
    assert_eq!(
        next(&mut output).await,
        Some("you said: second".to_string())
    );

    child.stop().await;
}

/// TC-PORT-PROC-23: closing the input is how a peer is told to go home, and it
/// is never signalled.
///
/// A peer on stdio has no shutdown request: end-of-input is the request. A
/// seam that went straight to a signal would kill servers mid-write and give
/// every peer author a reason to trap SIGTERM.
///
/// Input: a peer that exits 0 when its input ends.
/// Expected: `stop` answers the peer's own exit code, which is only possible
/// if it was allowed to exit on its own.
#[tokio::test]
async fn closing_the_input_is_how_a_peer_is_told_to_go_home() {
    let mut child = peer("while read -r line; do echo \"$line\"; done; exit 7")
        .spawn()
        .expect("the peer started");
    // Taken and dropped, which is what a consumer holding the writing half
    // does: the seam closes whatever is left.
    drop(child.stdin());

    assert_eq!(child.stop().await, PipedExit::Code(7));
}

/// TC-PORT-PROC-24: a peer that will not leave is ended with its whole
/// process group.
///
/// The reason this seam exists rather than a `spawn` in each consumer. A
/// consumer's own `child.kill()` ends the peer; a language server's indexer, a
/// server that shells out, a hook that starts a watcher - each is a
/// *grandchild*, and each survives, holding its pipes, sometimes for hours.
/// The group ladder is what makes "the peer is gone" mean "what the peer
/// started is gone".
///
/// Input: a peer that ignores SIGTERM and a closed input, having started a
/// long-lived helper of its own.
/// Expected: `stop` reports it was killed, and the helper is gone too.
#[tokio::test]
async fn a_peer_that_will_not_leave_is_ended_with_its_group() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pidfile = dir.path().join("helper.pid");
    let mut child = peer(&format!(
        "trap '' TERM; sleep 300 & echo $! > {}; echo ready; sleep 300",
        pidfile.display()
    ))
    .grace(Duration::from_millis(300))
    .spawn()
    .expect("the peer started");

    let mut output = BufReader::new(child.stdout().expect("its output")).lines();
    assert_eq!(next(&mut output).await, Some("ready".to_string()));
    let helper = read_pid(&pidfile);
    let peer_pid = child.pid().expect("a pid") as i32;

    assert_eq!(child.stop().await, PipedExit::Killed);

    assert!(!alive(peer_pid), "the peer itself outlived its stop");
    assert!(
        !alive(helper),
        "the peer's own child outlived it, which is the leak this seam exists to prevent"
    );
}

/// TC-PORT-PROC-25: stopping twice is not two stops.
///
/// A consumer may stop a peer from a shutdown path and again from a drop, and
/// the second must not signal: by then the pid may name a different process
/// altogether, and killing whatever inherited the number is the worst kind of
/// bug to diagnose.
///
/// Input: a peer stopped twice.
/// Expected: the first answers what happened to it, the second answers that
/// there was nothing left to stop.
#[tokio::test]
async fn stopping_twice_is_not_two_stops() {
    let mut child = peer("read -r _line || true")
        .spawn()
        .expect("the peer started");

    let first = child.stop().await;
    assert!(
        matches!(first, PipedExit::Code(_) | PipedExit::Killed),
        "the first stop says what happened: {first:?}"
    );
    assert_eq!(child.stop().await, PipedExit::Closed);
}

/// TC-PORT-PROC-26: the peer's environment is what the caller listed, and its
/// protocol stream is never inherited.
///
/// Both are seam-wide rules, and both matter more for a peer than for a
/// command: a server named in a settings document is a program a deployment
/// chose rather than one a model wrote, so handing it this process's
/// credentials is a decision nobody made - and a peer whose stdout was
/// inherited would print its frames onto the terminal and answer nobody.
///
/// Input: a peer given one variable, asked to print that variable and one this
/// process holds.
/// Expected: it reads its own variable off its own stdout, and the harness's
/// variable is empty in the child.
#[tokio::test]
async fn a_peer_gets_what_the_caller_listed_and_its_stdout_is_the_wire() {
    std::env::set_var("TETANUS_PIPED_SECRET", "not-for-the-peer");
    let mut child = peer("echo \"listed=$TETANUS_PIPED_LISTED inherited=$TETANUS_PIPED_SECRET\"")
        .env("TETANUS_PIPED_LISTED", "yes")
        .spawn()
        .expect("the peer started");

    let mut output = BufReader::new(child.stdout().expect("its output")).lines();
    assert_eq!(
        next(&mut output).await,
        Some("listed=yes inherited=".to_string()),
        "the child gets exactly what was listed, and reaches this process on its stdout"
    );

    child.stop().await;
    std::env::remove_var("TETANUS_PIPED_SECRET");
}

// ---------------------------------------------------------------- fixtures

/// A peer running `script`, quiet on stderr so a case's output is its own.
fn peer(script: &str) -> PipedCommand {
    PipedCommand::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .env("PATH", "/usr/bin:/bin")
        .diagnostics(Diagnostics::Discard)
        .grace(Duration::from_millis(500))
}

/// The next line a peer wrote, or `None` if it closed first. Bounded, so a
/// peer that never answers fails the case instead of hanging the suite.
async fn next<R>(lines: &mut tokio::io::Lines<BufReader<R>>) -> Option<String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("the peer answered within ten seconds")
        .expect("the pipe is readable")
}

/// The pid a case's peer recorded, waiting briefly for the file to appear.
fn read_pid(path: &std::path::Path) -> i32 {
    for _ in 0..300 {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("no pid was ever recorded at {}", path.display());
}

/// Whether a process still exists, asked the way a shell asks: signal zero.
fn alive(pid: i32) -> bool {
    for _ in 0..1_000 {
        // Safety: signal zero delivers nothing; it only asks whether the
        // process exists and could be signalled.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}
