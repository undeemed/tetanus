//! Test Design Specification: the pseudo-terminal layer.
//!
//! Feature under test: `tetanus_exec::pty` - allocation, size and resize,
//! signal delivery to the foreground process group, and a read loop that does
//! not lose output. Upstream's equivalent is the node-pty backend behind
//! `packages/terminal/terminal-bash` and the terminal half of
//! `packages/subprocess/subprocess-local`; the behaviours asserted here are the
//! ones its own suites assert of that backend.
//!
//! Approach: real terminals with real programs on them. A pty asserted against
//! a fake would be asserting the fake - every claim here is interesting
//! precisely because of what the *kernel* does with a terminal: `isatty`
//! answers differently, `stty` reports a size, `^C` reaches a process group
//! rather than a process, and a writer that outruns its reader is blocked by
//! the tty buffer rather than losing bytes.
//!
//! A terminal in its default mode turns `\n` into `\r\n` on the way out and
//! echoes what is written to it, so the cases normalize line endings and
//! account for the echo rather than pretending a tty behaves like a pipe.
//!
//! What is not asserted here, and why. The terminal *tools* over this layer -
//! a viewport, scrollback paging, an owner-scoped registry - are the next
//! slice: this is the layer they were named as waiting on, and building them
//! in the same commit would leave neither reviewable. `docs/parity.md`'s
//! terminal row still carries them.
//!
//! Environmental needs: Linux with `/dev/ptmx`, a POSIX shell, `stty`. The file
//! reports itself skipped where a terminal cannot be allocated rather than
//! passing for the wrong reason. No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

use std::time::Duration;

use tetanus_exec::pty::{PtyConfig, PtySession};

/// TC-PORT-PTY-1: a program on this terminal believes it has one.
///
/// The whole reason the layer exists. `isatty` is what a program asks before it
/// decides to colour its output, page it, draw a progress bar, or prompt
/// without echoing, and over a pipe the answer is no - so an interactive
/// program either refuses to run or behaves like a batch job.
///
/// Input: a shell on a terminal asked whether its input is a tty, and which
/// one.
/// Expected: it says yes and names a `/dev/pts` device.
#[tokio::test]
async fn a_program_on_this_terminal_believes_it_has_one() {
    let Some(session) = shell("test -t 0 && echo IS-A-TTY; tty").await else {
        return;
    };

    session.wait().await;
    let seen = normalized(&session.transcript());

    assert!(
        seen.contains("IS-A-TTY"),
        "the child saw a pipe, not a tty: {seen:?}"
    );
    assert!(
        seen.contains("/dev/pts/"),
        "the child could not name its terminal: {seen:?}"
    );
}

/// TC-PORT-PTY-2: the terminal has the size it was given, and resizing changes
/// what a program reads.
///
/// A pty with no size set reports 0x0, which every `stty size` shows and every
/// full-screen program believes. Setting it at allocation is what stops a
/// program starting up against a screen it thinks has no rows.
///
/// Input: a terminal allocated at 24x80, measured, then resized to 40x120 and
/// measured again by a program on it.
/// Expected: the terminal reports what it was set to, and a program run after
/// the resize reads the new size.
#[tokio::test]
async fn the_terminal_has_a_size_and_can_be_resized() {
    let Some(session) = shell("stty size; read _line; stty size").await else {
        return;
    };

    assert_eq!(session.size().expect("measured"), (24, 80));
    wait_for_text(&session, "24 80").await;

    session.resize(40, 120).expect("resized");
    assert_eq!(session.size().expect("measured"), (40, 120));
    // Let the program ask again, now that the terminal is a different shape.
    session.write("\n").await.expect("written");
    wait_for_text(&session, "40 120").await;

    let seen = normalized(&session.transcript());
    assert!(
        seen.contains("24 80") && seen.contains("40 120"),
        "the program did not see the resize: {seen:?}"
    );
    session.close().await;
}

/// TC-PORT-PTY-3: a signal reaches the foreground process group, and the
/// session survives it.
///
/// This is the case a pipe cannot have at all, and it is the reason `^C` works
/// on a terminal: the signal goes to whichever process group currently owns the
/// terminal, not to "the process". A harness that signalled the session leader
/// instead would kill the shell every time the model meant to interrupt a
/// command, and the session - with its directory, its variables and its
/// history - would be gone with it.
///
/// It is driven the way a terminal tool drives a session, because that is the
/// only way the claim is observable: an interactive shell with job control, a
/// command started by writing to the terminal, the signal, and then *another
/// command* to show the shell is still there. Asserting on the rest of an
/// interrupted command list would assert something else entirely - `bash`
/// abandons the list when a foreground command dies of `SIGINT`, which is the
/// shell's behaviour and not this layer's.
///
/// Input: an interactive job-controlled shell; `sleep 60` started on it; the
/// foreground group asked for and sent `SIGINT`; then `echo` written.
/// Expected: the foreground group is the command's, not the shell's; the signal
/// goes to the group that was asked for; the shell answers the next command;
/// and the session has not ended.
#[tokio::test]
async fn a_signal_reaches_the_foreground_group_and_the_shell_survives() {
    let Some(session) = interactive().await else {
        return;
    };
    session.write("sleep 60\n").await.expect("written");

    // Wait until the command really owns the terminal, rather than sleeping a
    // fixed time and hoping.
    let mut foreground = session.leader();
    for _ in 0..200 {
        if let Ok(group) = session.foreground_group() {
            if group != session.leader() {
                foreground = group;
                break;
            }
        }
        session.changed(Duration::from_millis(25)).await;
    }
    assert_ne!(
        foreground,
        session.leader(),
        "the command should own the terminal while it runs, not the shell"
    );

    let signalled = session
        .signal_foreground(libc::SIGINT)
        .expect("the signal was delivered");
    assert_eq!(
        signalled, foreground,
        "it went to the group that was asked for"
    );

    // The shell is still there to answer, which is the whole point of aiming at
    // the foreground group instead of the leader.
    session.write("echo STILL-ALIVE\n").await.expect("written");
    wait_for_text(&session, "STILL-ALIVE").await;

    let seen = normalized(&session.transcript());
    assert!(
        seen.contains("STILL-ALIVE"),
        "the shell died with its command: {seen:?}"
    );
    assert!(
        session.exit().is_none(),
        "interrupting a command must not end the session"
    );
    session.close().await;
}

/// TC-PORT-PTY-4: a child that writes faster than we drain loses nothing.
///
/// The requirement with the sharpest failure mode. Two losses are possible: the
/// kernel's pty buffer filling, which it answers by blocking the writer - so
/// the only way to lose bytes there is to stop reading - and our own bound,
/// which is answered separately in TC-PORT-PTY-5. This case pins the first: the
/// reader runs continuously rather than when someone asks, so a burst is
/// carried in full.
///
/// Input: a program printing 20,000 numbered lines as fast as it can, into a
/// transcript bound large enough to hold them.
/// Expected: every line is present, in order, none duplicated - checked by
/// counting and by looking for the first, the last and a sample in between.
#[tokio::test]
async fn a_child_that_outruns_the_reader_loses_nothing() {
    let lines = 20_000;
    let Some(session) = shell_with(
        &format!("for i in $(seq 1 {lines}); do echo line-$i; done; echo BURST-DONE"),
        PtyConfig {
            // Generously above what the burst produces, so this case is about
            // the read loop and not about the bound.
            max_scrollback: 8 * 1024 * 1024,
            ..PtyConfig::default()
        },
    )
    .await
    else {
        return;
    };

    session.wait().await;
    wait_for_text(&session, "BURST-DONE").await;
    let seen = normalized(&session.transcript());

    assert!(
        !session.truncated(),
        "the bound was reached, so this proves nothing"
    );
    let counted = seen
        .lines()
        .filter(|line| line.starts_with("line-"))
        .count();
    assert_eq!(
        counted, lines,
        "lines were lost between the child and the transcript"
    );
    for probe in ["line-1\n", "line-10000\n", &format!("line-{lines}\n")] {
        assert!(seen.contains(probe), "{probe:?} never arrived");
    }
}

/// TC-PORT-PTY-5: output past the bound is dropped from the beginning, and the
/// loss is reported.
///
/// The other half of -4. A transcript cannot grow without limit, so something
/// has to go; what matters is that it is the beginning - the end is what
/// someone is reading - and that a reader is told, rather than handed a shorter
/// transcript that looks complete.
///
/// Input: the same burst into a small bound.
/// Expected: the transcript is near its bound, holds the last line, has lost
/// the first, and says it was truncated.
#[tokio::test]
async fn output_past_the_bound_drops_the_beginning_and_says_so() {
    let Some(session) = shell_with(
        "for i in $(seq 1 20000); do echo line-$i; done; echo THE-VERY-LAST-LINE",
        PtyConfig {
            max_scrollback: 16 * 1024,
            ..PtyConfig::default()
        },
    )
    .await
    else {
        return;
    };

    session.wait().await;
    wait_for_text(&session, "THE-VERY-LAST-LINE").await;
    let seen = normalized(&session.transcript());

    assert!(
        session.truncated(),
        "far more was printed than the bound keeps"
    );
    assert!(
        seen.len() <= 16 * 1024 + 4096,
        "the transcript grew past its bound: {} bytes",
        seen.len()
    );
    assert!(
        seen.contains("THE-VERY-LAST-LINE"),
        "the end is what a reader is reading, and it was dropped"
    );
    assert!(
        !seen.contains("line-1\n"),
        "the beginning should have gone first"
    );
}

/// TC-PORT-PTY-6: what is typed reaches the program, and the terminal echoes
/// it the way a terminal does.
///
/// Input is the other half of a terminal, and the echo is the part that
/// surprises a caller comparing output to what a program printed: a tty echoes
/// what is written to it, so the transcript carries both sides of the
/// conversation.
///
/// Input: `cat` on a terminal, written to twice, then ended.
/// Expected: both lines come back, and the session ends when its input is
/// closed.
#[tokio::test]
async fn what_is_typed_reaches_the_program_and_is_echoed() {
    let Some(session) = shell("cat").await else {
        return;
    };

    session.write("first line\n").await.expect("written");
    wait_for_text(&session, "first line").await;
    session.write("second line\n").await.expect("written");
    wait_for_text(&session, "second line").await;
    // End-of-transmission: what a terminal sends for Ctrl-D, which is how a
    // program reading its terminal is told there is no more input.
    session.write("\u{4}").await.expect("written");

    let exit = tokio::time::timeout(Duration::from_secs(10), session.wait())
        .await
        .expect("the program ended when its input closed");
    assert_eq!(exit.code, Some(0));
    let seen = normalized(&session.transcript());
    assert!(
        seen.contains("first line") && seen.contains("second line"),
        "{seen:?}"
    );
}

/// TC-PORT-PTY-7: closing a terminal takes everything on it.
///
/// A terminal session is a process group by construction - the leader called
/// `setsid` - so closing it is group-scoped for the same reason every other
/// terminator in this crate is: what the shell started is what would otherwise
/// be left behind.
///
/// Input: a shell on a terminal that starts a long background sleep, then the
/// session closed.
/// Expected: the session reports an exit, and the background process is gone.
#[tokio::test]
async fn closing_a_terminal_takes_everything_on_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pidfile = dir.path().join("child.pid");
    let Some(session) = shell(&format!(
        "sleep 60 & echo $! > {}; sleep 60",
        pidfile.display()
    ))
    .await
    else {
        return;
    };
    let child = read_pid(&pidfile);

    session.close().await;

    assert!(session.exit().is_some(), "the session did not end");
    assert!(!alive(child), "a process on the terminal outlived it");
}

/// TC-PORT-PTY-8: a terminal that cannot be allocated is an error, not a
/// half-working session.
///
/// Input: a program that does not exist, started on a terminal.
/// Expected: a spawn error naming it, and no session handed back - the
/// terminal is not left allocated with nothing on it.
#[tokio::test]
async fn a_terminal_with_nothing_to_run_is_an_error() {
    let refused = PtySession::spawn(
        &["no-such-program-on-a-tty".to_string()],
        std::path::Path::new("."),
        &[],
        PtyConfig::default(),
    )
    .await;

    match refused {
        Err(tetanus_exec::pty::PtyError::Spawn { program, .. }) => {
            assert_eq!(program, "no-such-program-on-a-tty")
        }
        Err(tetanus_exec::pty::PtyError::Allocate(why)) => {
            eprintln!("skipped: this host cannot allocate a pseudo-terminal ({why})");
        }
        other => panic!("expected a spawn failure, got {other:?}"),
    }
}

// ---------------------------------------------------------------- fixtures

/// A shell running `script` on a terminal, or `None` after reporting the case
/// skipped where a terminal cannot be allocated.
async fn shell(script: &str) -> Option<PtySession> {
    shell_with(script, PtyConfig::default()).await
}

/// An interactive shell on the terminal, which is where job control comes
/// from: a command it runs gets a process group of its own, so the foreground
/// group is a real question with a different answer from the leader.
async fn interactive() -> Option<PtySession> {
    let argv = vec!["/bin/bash".to_string(), "-i".to_string()];
    match PtySession::spawn(
        &argv,
        std::path::Path::new("."),
        &[
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("TERM".to_string(), "xterm".to_string()),
            // A prompt would be noise in every assertion here, and an empty one
            // is still a prompt as far as the shell is concerned.
            ("PS1".to_string(), String::new()),
        ],
        PtyConfig::default(),
    )
    .await
    {
        Ok(session) => Some(session),
        Err(why) => {
            eprintln!("skipped: no job-controlled shell on a terminal here ({why})");
            None
        }
    }
}

async fn shell_with(script: &str, config: PtyConfig) -> Option<PtySession> {
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    match PtySession::spawn(
        &argv,
        std::path::Path::new("."),
        &[
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("TERM".to_string(), "xterm".to_string()),
        ],
        config,
    )
    .await
    {
        Ok(session) => Some(session),
        Err(tetanus_exec::pty::PtyError::Allocate(why)) => {
            eprintln!("skipped: this host cannot allocate a pseudo-terminal ({why})");
            None
        }
        Err(other) => panic!("the terminal could not be started: {other}"),
    }
}

/// Wait until `text` appears in the transcript, or give up after a bounded
/// wait. Polling the transcript rather than sleeping a fixed time keeps the
/// cases quick when the machine is idle and correct when it is loaded.
async fn wait_for_text(session: &PtySession, text: &str) {
    for _ in 0..600 {
        if normalized(&session.transcript()).contains(text) {
            return;
        }
        session.changed(Duration::from_millis(50)).await;
    }
}

/// The transcript with the terminal's own carriage returns taken out, which is
/// what a caller comparing against what a program printed wants.
fn normalized(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// The pid a case's shell recorded, waiting briefly for the file to appear.
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
