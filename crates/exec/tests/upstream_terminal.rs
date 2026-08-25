//! Test Design Specification: persistent shells, ported.
//!
//! Feature under test: `tetanus_exec::session` - a long-lived shell a turn
//! reuses across tool calls, its lifecycle, and what it does when the shell
//! dies. Upstream pins the same decisions in
//! `packages/terminal/terminal-bash/tests/session.spec.ts`,
//! `packages/terminal/terminal/tests/service.spec.ts` and
//! `packages/shell/tool-bash-persistent/tests/tools.spec.ts`.
//!
//! Approach: real shells. A persistent shell asserted against a fake would be
//! asserting the fake: the whole claim is that an operating-system process
//! keeps its own state, and the interesting failures are the ones a pipe, a
//! marker and a dying process produce.
//!
//! What upstream has here that this does not, and why. Upstream's sessions are
//! PTYs, so it can assert on a viewport, a scrollback page, terminal
//! dimensions, an `stty`-visible terminal, prompt-based readiness, foreground
//! process-group inspection, and signalling the foreground group with SIGINT
//! while the shell survives. A shell reading a pipe has no terminal, so those
//! have nothing to restate: there is no viewport (there is a transcript), no
//! foreground group distinct from the session's group, and no readiness to
//! infer from a prompt (the marker is exact where a prompt is a guess). What
//! carries over is what a model can observe: state surviving between calls,
//! the exit status of each command, a bounded transcript, one command at a
//! time, and a death that is reported. Upstream's owner-scoping (a session
//! belongs to one agent, and another agent asking for it is told it does not
//! exist) has no counterpart until sessions are owned by an agent identity;
//! `docs/parity.md` carries it.
//!
//! Environmental needs: a bash on PATH and a writable temp directory. No case
//! reaches a network or an API key. The file is skipped off unix, because the
//! process-group termination under it is POSIX.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use tetanus_exec::backend::{Bash, ShellBackend};
use tetanus_exec::proc::Collected;
use tetanus_exec::session::{Gone, SessionConfig, SessionError, ShellSession, ShellSessions};

/// TC-PORT-TERM-1: a session keeps its directory and its variables between
/// calls.
///
/// Upstream: "state persists across sends", "cwd survives", "exported
/// variables survive" (`tool-bash-persistent`).
///
/// This is the whole reason a persistent shell exists. Without it a model has
/// to restate `cd` in every command it writes, and every multi-step piece of
/// work becomes one enormous command line.
///
/// Input: one session; `cd` and `export` in the first call, `pwd` and the
/// variable in the second.
/// Expected: the second call sees both, and reports exit 0.
#[tokio::test]
async fn a_session_keeps_its_directory_and_variables_between_calls() {
    let dir = tempfile::tempdir().expect("temp dir");
    let inner = dir.path().join("inner");
    std::fs::create_dir(&inner).expect("made");
    let sessions = ShellSessions::new();
    let session = open(&sessions, dir.path()).await;

    let first = session
        .run(&format!("cd {}; export MARK=carried", inner.display()))
        .await
        .expect("the first command ran");
    assert_eq!(first.code, 0);

    let second = session
        .run("pwd; echo \"$MARK\"")
        .await
        .expect("the second command ran");

    assert_eq!(second.code, 0);
    let seen: Vec<&str> = second.text.lines().collect();
    assert_eq!(
        seen,
        vec![
            std::fs::canonicalize(&inner)
                .expect("canonical")
                .display()
                .to_string()
                .as_str(),
            "carried"
        ],
        "the second call ran in the shell the first call left behind"
    );
}

/// TC-PORT-TERM-2: each command's own output is what comes back, with its own
/// exit status.
///
/// Upstream: "returns only the output of the command just sent", "reports the
/// exit code of each command".
///
/// The marker protocol exists for this. A transcript is one long stream, and a
/// caller that handed back the whole thing would give the model the previous
/// command's output again on every call.
///
/// Input: three commands on one session, one of which fails.
/// Expected: each answer holds only its own output; the failing one reports
/// its code and does not become an error; nothing of the protocol's own
/// markers is in any answer.
#[tokio::test]
async fn each_command_gets_its_own_output_and_status() {
    let dir = tempfile::tempdir().expect("temp dir");
    let sessions = ShellSessions::new();
    let session = open(&sessions, dir.path()).await;

    let first = session.run("echo one").await.expect("ran");
    assert_eq!(first.text, "one");
    assert_eq!(first.code, 0);

    let failing = session
        .run("echo two; exit_code=3; (exit $exit_code)")
        .await
        .expect("ran");
    assert_eq!(failing.text, "two");
    assert_eq!(
        failing.code, 3,
        "a failing command is a result, not an error"
    );

    let third = session.run("echo three").await.expect("ran");
    assert_eq!(third.text, "three", "no earlier output came back with it");
    for answer in [&first.text, &failing.text, &third.text] {
        assert!(
            !answer.contains("__tetanus"),
            "the protocol's markers leaked into the model's view: {answer:?}"
        );
    }
}

/// TC-PORT-TERM-3: stderr and stdout arrive in the order the shell wrote them.
///
/// Upstream gets this from a PTY, where both streams share one device. A
/// session here gets it from the shell itself: its setup puts its stderr onto
/// its stdout, so the ordering is the shell's rather than a race between two
/// pipes.
///
/// Input: a command interleaving the two streams.
/// Expected: the four lines in the order the command printed them.
#[tokio::test]
async fn both_streams_arrive_in_the_order_the_shell_wrote_them() {
    let dir = tempfile::tempdir().expect("temp dir");
    let sessions = ShellSessions::new();
    let session = open(&sessions, dir.path()).await;

    let ran = session
        .run("echo a; echo b 1>&2; echo c; echo d 1>&2")
        .await
        .expect("ran");

    assert_eq!(
        ran.text.lines().collect::<Vec<_>>(),
        vec!["a", "b", "c", "d"]
    );
}

/// TC-PORT-TERM-4: the lifecycle is open, run, close - and close means gone.
///
/// Upstream: `terminal_open` / `terminal_send` / `terminal_close`, and
/// "closing waits until the captured owned process tree is gone".
///
/// Input: a session that starts a background child, then is closed.
/// Expected: the session is listed while open and not after; a command after
/// the close is refused with the reason; and the child the shell started is
/// gone, because closing kills the process group rather than the shell alone.
#[tokio::test]
async fn the_lifecycle_is_open_run_close_and_close_means_gone() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pidfile = dir.path().join("child.pid");
    let sessions = ShellSessions::new();
    let session = open(&sessions, dir.path()).await;

    session
        .run(&format!("sleep 30 & echo $! > {}", pidfile.display()))
        .await
        .expect("ran");
    assert_eq!(sessions.list().len(), 1);
    let id = session.id().to_string();

    sessions.close(&id).await.expect("closed");

    assert!(sessions.list().is_empty(), "a closed session is not listed");
    assert!(
        matches!(sessions.get(&id), Err(SessionError::Unknown(_))),
        "and it cannot be fetched again"
    );
    let refused = session.run("echo after").await.expect_err("it is closed");
    assert!(
        matches!(
            refused,
            SessionError::Gone {
                reason: Gone::Closed,
                ..
            }
        ),
        "a call on a closed session says so: {refused}"
    );
    assert!(
        !alive(read_pid(&pidfile)),
        "closing a session kills what its shell started"
    );
}

/// TC-PORT-TERM-5: a shell that dies is reported, and is not restarted
/// underneath the caller.
///
/// Upstream restarts and prints "the persistent bash shell was reset"; this
/// keeps the notice and drops the restart, and this case is the difference.
///
/// A silent restart hands the model a shell in a state it did not create: the
/// directory it changed to is gone, the variables it exported are gone, and
/// the next command runs somewhere it did not choose while the transcript says
/// everything succeeded. Being told is strictly more useful, because opening a
/// new session is one call.
///
/// Input: a command that exits the shell, and then two more calls.
/// Expected: the command that killed it reports the death and carries what it
/// printed first; every later call reports the same reason; and no new shell
/// appears - the session is still the one that died.
#[tokio::test]
async fn a_shell_that_dies_is_reported_and_not_restarted() {
    let dir = tempfile::tempdir().expect("temp dir");
    let sessions = ShellSessions::new();
    let session = open(&sessions, dir.path()).await;

    session.run("export BEFORE=set").await.expect("ran");
    let died = session
        .run("echo last words; exit 9")
        .await
        .expect_err("the shell exited under the command");

    match &died {
        SessionError::Died { reason, partial } => {
            assert_eq!(
                *reason,
                Gone::Exited {
                    code: Some(9),
                    signal: None
                }
            );
            assert!(
                partial.contains("last words"),
                "what it printed first is the evidence: {partial:?}"
            );
        }
        other => panic!("expected a reported death, got {other}"),
    }

    let after = session
        .run("echo \"$BEFORE\"")
        .await
        .expect_err("still gone");
    assert!(
        matches!(after, SessionError::Gone { .. }),
        "a dead session stays dead: {after}"
    );
    assert!(
        session.gone().is_some(),
        "and it says so without being asked to run anything"
    );
    assert_eq!(
        sessions.list().len(),
        1,
        "nothing restarted it behind the caller's back"
    );
}

/// TC-PORT-TERM-6: a command that hangs takes its budget, and the session with
/// it.
///
/// Upstream: "a command that exceeds the deadline returns partial output and
/// resets the shell".
///
/// A shell still running a command nobody is waiting for cannot be reused: the
/// next command would be read by the hung one's stdin, or interleave with its
/// output. So the budget ends the session, and the caller is told - with what
/// the command printed before it hung, which is usually the reason.
///
/// Input: a session with a short budget, and a command that prints and sleeps.
/// Expected: `TimedOut` naming the budget, carrying the early output; the
/// session is gone afterwards; and the shell's whole process group is gone
/// with it.
#[tokio::test]
async fn a_command_that_hangs_takes_its_budget_and_the_session() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pidfile = dir.path().join("hang.pid");
    let sessions = ShellSessions::new();
    let session = sessions
        .open(
            Arc::new(Bash::new()),
            SessionConfig {
                cwd: dir.path().to_path_buf(),
                timeout: Duration::from_millis(400),
                grace: Duration::from_millis(200),
                ..SessionConfig::default()
            },
        )
        .await
        .expect("a session starts");

    let started = std::time::Instant::now();
    let refused = session
        .run(&format!(
            "echo before hanging; sleep 60 & echo $! > {}; wait",
            pidfile.display()
        ))
        .await
        .expect_err("it ran past its budget");

    assert!(
        started.elapsed() < Duration::from_secs(20),
        "it waited for the sleep"
    );
    match &refused {
        SessionError::TimedOut { after, partial } => {
            assert_eq!(*after, Duration::from_millis(400));
            assert!(
                partial.contains("before hanging"),
                "the early output is carried: {partial:?}"
            );
        }
        other => panic!("expected a timeout, got {other}"),
    }
    assert_eq!(
        session.gone(),
        Some(Gone::TimedOut {
            after: Duration::from_millis(400)
        })
    );
    assert!(
        !alive(read_pid(&pidfile)),
        "the command the budget ended is gone, and so is what it started"
    );
}

/// TC-PORT-TERM-7: a long-running command is readable while it runs.
///
/// Upstream: `readOutput()` on a live send ("consume output produced since the
/// prior call", "consecutive reads never repeat").
///
/// Input: a command printing three lines a beat apart, with a sink attached.
/// Expected: the first line reaches the sink well before the command finishes;
/// every line arrives exactly once; and no marker is delivered, because the
/// protocol is not the command's output.
#[tokio::test]
async fn a_long_running_command_is_readable_while_it_runs() {
    let dir = tempfile::tempdir().expect("temp dir");
    let sessions = ShellSessions::new();
    let session = open(&sessions, dir.path()).await;
    let sink = Arc::new(Collected::new());

    let watcher = {
        let sink = Arc::clone(&sink);
        tokio::spawn(async move {
            for _ in 0..200 {
                if sink.text().contains("tick-1") {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        })
    };

    let ran = session
        .run_with(
            "echo tick-1; sleep 1; echo tick-2; sleep 1; echo tick-3",
            Some(sink.clone()),
        )
        .await
        .expect("ran");

    assert!(
        watcher.await.expect("the watcher ran"),
        "the first line never arrived before the command ended"
    );
    assert_eq!(
        ran.text.lines().collect::<Vec<_>>(),
        vec!["tick-1", "tick-2", "tick-3"]
    );
    let streamed = sink.text();
    for line in ["tick-1", "tick-2", "tick-3"] {
        assert_eq!(
            streamed.matches(line).count(),
            1,
            "{line} was delivered {} times",
            streamed.matches(line).count()
        );
    }
    assert!(
        !streamed.contains("__tetanus"),
        "the protocol's markers reached the sink: {streamed:?}"
    );
}

/// TC-PORT-TERM-8: two sessions are two shells.
///
/// Upstream: "sessions are isolated", "each session has its own id".
///
/// Input: two sessions, each setting the same variable to a different value.
/// Expected: neither sees the other's; each has its own id; and listing
/// reports both.
#[tokio::test]
async fn two_sessions_are_two_shells() {
    let dir = tempfile::tempdir().expect("temp dir");
    let sessions = ShellSessions::new();
    let first = open(&sessions, dir.path()).await;
    let second = open(&sessions, dir.path()).await;

    assert_ne!(first.id(), second.id());
    first.run("export WHO=first").await.expect("ran");
    second.run("export WHO=second").await.expect("ran");

    assert_eq!(first.run("echo \"$WHO\"").await.expect("ran").text, "first");
    assert_eq!(
        second.run("echo \"$WHO\"").await.expect("ran").text,
        "second"
    );
    assert_eq!(sessions.list().len(), 2);
}

/// TC-PORT-TERM-9: one command at a time on one shell.
///
/// Upstream: "exactly one send may be active per PTY session".
///
/// Two commands written into one shell would interleave their markers and
/// their output, and neither answer could be attributed to either command.
/// The guard is inside the session rather than left to the caller, because a
/// tool pipeline that dispatches in parallel is a caller that will get this
/// wrong.
///
/// Input: two commands started at once on one session, each printing its own
/// name.
/// Expected: each answer holds only its own output, and the two ran one after
/// the other rather than at the same time.
#[tokio::test]
async fn one_command_at_a_time_on_one_shell() {
    let dir = tempfile::tempdir().expect("temp dir");
    let sessions = ShellSessions::new();
    let session = open(&sessions, dir.path()).await;

    let one = session.run_with("sleep 0.3; echo alpha", None);
    let two = session.run_with("echo beta", None);
    let (first, second) = tokio::join!(one, two);

    let first = first.expect("ran");
    let second = second.expect("ran");
    assert_eq!(first.text, "alpha");
    assert_eq!(
        second.text, "beta",
        "the second answer is only its own output"
    );
}

/// TC-PORT-TERM-47: what a session's bound drops is kept, and the result says
/// where.
///
/// The other half of TC-PORT-TERM-10. A bounded transcript is right - a
/// session that ran for an hour must not be an hour of resident memory - but
/// "the beginning is gone" is a poor answer for the commands that reach the
/// bound at all, which are the builds, the test runs and the log tails whose
/// beginnings are exactly where the first error is.
///
/// `crate::shell` has kept the whole of a one-shot command's output since the
/// spill store landed, and the argument transfers without change: only the
/// producer holds the bytes it is dropping. By the time a result exists the
/// prefix is gone, so nothing above this seam can file it.
///
/// Input: one session with a small bound and a spill store; a command printing
/// far past it; then a command that fits.
/// Expected: the big command's result is bounded and names an artifact; the
/// artifact holds the first line the result no longer has, and the last;
/// the command that fits names nothing and leaves no artifact behind.
#[tokio::test]
async fn what_a_sessions_bound_drops_is_kept_and_the_result_says_where() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("artifacts");
    let sessions = ShellSessions::new();
    let session = sessions
        .open(
            Arc::new(Bash::new()),
            SessionConfig {
                cwd: dir.path().to_path_buf(),
                max_scrollback: 4096,
                spill: Some(tetanus_exec::shell::SpillTo {
                    store: Arc::new(tetanus_core::spill::SpillStore::at(&root)),
                    session: "a-session".to_string(),
                }),
                ..SessionConfig::default()
            },
        )
        .await
        .expect("a session starts");

    let big = session
        .run("for i in $(seq 1 3000); do echo line-$i; done; echo THE-LAST-LINE")
        .await
        .expect("ran");

    assert!(big.truncated, "far more was printed than the bound keeps");
    assert!(
        !big.text.contains("line-1\n"),
        "the result keeps the tail, so the first line is gone from it"
    );
    let locator = big.spilled.as_ref().expect("the whole of it was kept");
    let whole = std::fs::read_to_string(locator).expect("the artifact is readable");
    assert!(
        whole.contains("line-1\n") && whole.contains("THE-LAST-LINE"),
        "the artifact is this command's whole output, not the part after the bound was hit"
    );
    assert_eq!(
        whole
            .lines()
            .filter(|line| line.starts_with("line-"))
            .count(),
        3000,
        "every line exactly once"
    );

    let small = session.run("echo just-this").await.expect("ran");
    assert!(!small.truncated);
    assert_eq!(
        small.spilled, None,
        "a command that fits is not filed anywhere"
    );
    let filed = std::fs::read_dir(root.join("session-a-session"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(filed, 1, "one artifact for the one command that needed it");
}

/// TC-PORT-TERM-10: the transcript is bounded, and the bound keeps the end.
///
/// Upstream: a backend-owned scrollback bound, with `truncated` reported.
///
/// A session that ran for an hour must not be an hour of resident memory, and
/// what is dropped is the beginning: a model reading a session is reading what
/// just happened.
///
/// Input: a session with a small bound, and a command printing far more.
/// Expected: the retained transcript is near the bound, holds the last line,
/// and the answer says it was truncated.
#[tokio::test]
async fn the_transcript_is_bounded_and_the_bound_keeps_the_end() {
    let dir = tempfile::tempdir().expect("temp dir");
    let sessions = ShellSessions::new();
    let session = sessions
        .open(
            Arc::new(Bash::new()),
            SessionConfig {
                cwd: dir.path().to_path_buf(),
                max_scrollback: 4096,
                ..SessionConfig::default()
            },
        )
        .await
        .expect("a session starts");

    let ran = session
        .run("for i in $(seq 1 3000); do echo padding-line-$i; done; echo THE-LAST-LINE")
        .await
        .expect("ran");

    assert!(ran.truncated, "far more was printed than the bound keeps");
    assert!(
        ran.text.trim_end().ends_with("THE-LAST-LINE"),
        "the end is what was kept: {:?}",
        ran.text.chars().rev().take(40).collect::<String>()
    );
    assert!(
        session.transcript().len() <= 4096 + 1024,
        "the transcript grew past its bound: {} bytes",
        session.transcript().len()
    );
}

/// TC-PORT-TERM-11: a session on a backend this host does not have is refused
/// at open.
///
/// Upstream: a backend type that is not registered is an error from `spawn`.
///
/// Input: a session asked for on a bash pinned to a program that is not there.
/// Expected: `Backend`, naming the missing program; nothing is published, so
/// nothing has to be cleaned up.
#[tokio::test]
async fn a_session_on_a_missing_backend_is_refused_at_open() {
    let sessions = ShellSessions::new();

    let refused = sessions
        .open(
            Arc::new(Bash::at("/nowhere/bin/bash")),
            SessionConfig::default(),
        )
        .await
        .expect_err("that shell is not on this host");

    assert!(matches!(refused, SessionError::Backend(_)), "{refused}");
    assert!(
        sessions.list().is_empty(),
        "a session that never started is not published"
    );
}

/// TC-PORT-TERM-12: closing everything closes everything.
///
/// Upstream: "disposal terminates all still-running managed processes and
/// awaits their exit".
///
/// Input: three sessions, each with a background child, then `close_all`.
/// Expected: nothing is listed and every child is gone.
#[tokio::test]
async fn closing_everything_closes_everything() {
    let dir = tempfile::tempdir().expect("temp dir");
    let sessions = ShellSessions::new();
    let mut pidfiles = Vec::new();
    for index in 0..3 {
        let session = open(&sessions, dir.path()).await;
        let pidfile = dir.path().join(format!("child-{index}.pid"));
        session
            .run(&format!("sleep 30 & echo $! > {}", pidfile.display()))
            .await
            .expect("ran");
        pidfiles.push(pidfile);
    }

    sessions.close_all().await;

    assert!(sessions.list().is_empty());
    for pidfile in pidfiles {
        assert!(!alive(read_pid(&pidfile)), "a child outlived the teardown");
    }
}

/// A bash session in `cwd`, with the default budget.
async fn open(sessions: &ShellSessions, cwd: &std::path::Path) -> Arc<ShellSession> {
    let backend: Arc<dyn ShellBackend> = Arc::new(Bash::new());
    sessions
        .open(
            backend,
            SessionConfig {
                cwd: cwd.to_path_buf(),
                grace: Duration::from_millis(200),
                ..SessionConfig::default()
            },
        )
        .await
        .expect("this host has a bash")
}

/// The pid a case's shell recorded, waiting briefly for the file to appear.
fn read_pid(path: &std::path::Path) -> i32 {
    for _ in 0..200 {
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
///
/// Waits for the answer rather than sampling it, because a kill is delivered
/// asynchronously and the claim under test is that the process dies - not that
/// it has died by the time the next line runs. The window is long because a
/// loaded machine schedules the reaper late, and a case that fails only under
/// load is a case nobody can read.
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
