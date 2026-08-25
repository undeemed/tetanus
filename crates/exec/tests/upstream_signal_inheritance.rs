//! Test Design Specification: a child starts with a signal disposition of its
//! own.
//!
//! Feature under test: `tetanus_exec::signals`, applied between `fork` and
//! `exec` by every child this crate starts. The property is one sentence: an
//! interrupt reaches a command *whatever the harness itself was started with*.
//!
//! Why it exists, which is the part worth reading. A signal set to `SIG_IGN`
//! is inherited across `fork` **and** across `exec`, and POSIX has a shell set
//! `SIGINT` to `SIG_IGN` for any command it runs in the background - which is
//! how `tetanus serve &`, a systemd unit, a CI job and an orchestrator all
//! start a harness. Everything downstream inherited it: the shell on the
//! pseudo-terminal, and every command that shell ran. `killpg` kept returning
//! success, because delivery *did* succeed; the process simply ignored it. So
//! `terminal_signal` reported `delivered SIGINT to foreground process group N`
//! while `sleep 30` slept on, and the turn's interrupt stopped nothing.
//!
//! It was found as three tests that failed under load and passed in isolation.
//! Load was a correlation, not the cause: a busy machine is also a machine
//! being driven from a script rather than by hand. The same code, the same
//! machine, no load at all - run from an interactive shell the `sleep` dies,
//! run with `&` it survives.
//!
//! Approach: reproduce the *cause* rather than the correlation. The case sets
//! `SIGINT` to `SIG_IGN` in this process, which is exactly what a background
//! launch does, and then asserts a command on a terminal still dies. Before
//! the fix it survives; after it, it does not. No load, no sleeping, no
//! second process to arrange.
//!
//! The cases here mutate process-wide signal state, so they hold a mutex
//! against each other and nothing else shares their binary: cargo gives one
//! binary per file, and within it these two are the only tests.
//!
//! Environmental needs: Linux with `/dev/ptmx` and a bash on PATH.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::time::Duration;

use tetanus_exec::backend::Bash;
use tetanus_exec::terminal::{TerminalConfig, TerminalError, TerminalSession, TerminalSignal};

/// Held by both cases: each changes this process's signal state, and two of
/// them doing it at once would leave neither measuring what it claims to.
///
/// An async mutex rather than a `std` one because the guard is held across
/// awaits by construction - the whole body of each case is what has to be
/// serialised, and that body drives a terminal.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// TC-PORT-TERM-44: a harness whose own `SIGINT` is ignored still interrupts
/// the command it started.
///
/// Input: this process with `SIGINT` set to `SIG_IGN` - what a shell does to a
/// command it runs in the background - then a terminal session, a `sleep 300`
/// typed into it, and the signal the model would send.
/// Expected: the command dies and the shell takes the terminal back. Without
/// `signals::reset_for_child` the `killpg` succeeds, the sleep ignores it, and
/// the foreground group is still the command's a full ten seconds later.
#[tokio::test]
async fn a_harness_that_ignores_sigint_still_interrupts_its_own_commands() {
    let _serial = ONE_AT_A_TIME.lock().await;
    // What a background launch leaves behind. Restored at the end, because a
    // disposition is process-wide and the next case in this binary - or a
    // future one - should not inherit this one's.
    let previous = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    assert_ne!(previous, libc::SIG_ERR, "the case could not arrange itself");

    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal(workspace.path()).await else {
        unsafe { libc::signal(libc::SIGINT, previous) };
        return;
    };
    let session = Arc::new(session);

    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.send("sleep 300", true, None).await }
    });
    // Wait for the command to own the terminal rather than assuming it does:
    // the case is about what the signal reaches, not about how quickly a shell
    // forks.
    let mut command = session.pid();
    for _ in 0..400 {
        match session.foreground_group() {
            Ok(group) if group != session.pid() => {
                command = group;
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
    assert_ne!(
        command,
        session.pid(),
        "the command never took the terminal, so this case would prove nothing"
    );

    let reached = session
        .signal(TerminalSignal::Int)
        .expect("the signal was delivered");
    assert_eq!(reached, command, "it went to the command's own group");

    // The claim: delivered *and* obeyed. A process that inherited the ignore
    // is signalled just as successfully and does not die, which is what made
    // this look like flakiness for as long as nobody checked.
    let mut gone = false;
    for _ in 0..400 {
        if unsafe { libc::kill(command, 0) } != 0 {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        gone,
        "the command ignored an interrupt it was successfully sent: it inherited this \
         process's disposition instead of starting with its own"
    );

    let settled = tokio::time::timeout(Duration::from_secs(20), running)
        .await
        .expect("the send answered once its command died")
        .expect("the task");
    assert!(settled.is_ok(), "the session survived its own interrupt");
    assert_eq!(session.status(), tetanus_exec::terminal::Status::Running);

    session.close().await;
    unsafe { libc::signal(libc::SIGINT, previous) };
}

/// TC-PORT-TERM-48: a harness whose own `SIGINT` is *blocked* still interrupts
/// the command it started.
///
/// The other half of the same inheritance, and the half that shipped without
/// evidence: `signals::reset_for_child` empties the child's signal mask as
/// well as resetting its dispositions, and only the dispositions had a case.
///
/// A blocked signal is inherited across `exec` exactly as an ignored one is,
/// and it is just as ordinary a thing to inherit: a supervisor that blocks
/// signals while it forks, a process started from inside a signal handler, a
/// runtime that masks during startup. The visible failure is identical -
/// `killpg` reports success, the command sleeps on - and so is what it costs:
/// a model asks for its runaway command to stop, and is told that it did.
///
/// Input: this process with `SIGINT` blocked rather than ignored, then a
/// terminal, a `sleep 300`, and the signal a model would send.
/// Expected: the command dies. With the mask half of the fix removed it does
/// not, though the disposition half is still in place.
#[tokio::test]
async fn a_harness_that_blocks_sigint_still_interrupts_its_own_commands() {
    let _serial = ONE_AT_A_TIME.lock().await;

    // Block `SIGINT` here, keeping what was blocked before so the process is
    // left as it was found.
    let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
    let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGINT);
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous),
            0,
            "the case could not arrange itself"
        );
    }
    let restore = || unsafe {
        libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
    };

    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal(workspace.path()).await else {
        restore();
        return;
    };
    let session = Arc::new(session);

    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.send("sleep 300", true, None).await }
    });
    let mut command = session.pid();
    for _ in 0..400 {
        match session.foreground_group() {
            Ok(group) if group != session.pid() => {
                command = group;
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
    assert_ne!(
        command,
        session.pid(),
        "the command never took the terminal, so this case would prove nothing"
    );

    session
        .signal(TerminalSignal::Int)
        .expect("the signal was delivered");

    let mut gone = false;
    for _ in 0..400 {
        if unsafe { libc::kill(command, 0) } != 0 {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        gone,
        "the command inherited this process's blocked mask, so a signal it was \
         successfully sent was never delivered to it"
    );

    let settled = tokio::time::timeout(Duration::from_secs(20), running)
        .await
        .expect("the send answered once its command died")
        .expect("the task");
    assert!(settled.is_ok(), "the session survived its own interrupt");

    session.close().await;
    restore();
}

/// A terminal session on this host's bash, or `None` after reporting the case
/// skipped where there is no terminal to allocate.
async fn terminal(workspace: &std::path::Path) -> Option<TerminalSession> {
    match TerminalSession::open(
        "pty-1".into(),
        None,
        "bash".into(),
        Arc::new(Bash::new()),
        TerminalConfig {
            cwd: workspace.to_path_buf(),
            idle_silence: Duration::from_secs(30),
            timeout: Duration::from_secs(30),
            grace: Duration::from_millis(200),
            ..TerminalConfig::default()
        },
    )
    .await
    {
        Ok(session) => Some(session),
        Err(TerminalError::Pty(tetanus_exec::pty::PtyError::Allocate(why))) => {
            eprintln!("skipped: this host cannot allocate a pseudo-terminal ({why})");
            None
        }
        Err(TerminalError::Backend(why)) => {
            eprintln!("skipped: this host has no bash ({why})");
            None
        }
        Err(other) => panic!("the terminal session could not be opened: {other}"),
    }
}
