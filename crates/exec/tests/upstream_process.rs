//! Test Design Specification: running one external command, ported.
//!
//! Feature under test: `tetanus_exec::proc::Command` - the primitive under
//! everything that has to leave the process. Upstream pins the same behaviour
//! in `packages/subprocess/subprocess-local/tests/spawn.spec.ts` and
//! `process-exit.spec.ts`; each case names the upstream case it comes from.
//!
//! Approach: real processes, via `/bin/sh` and the coreutils every POSIX host
//! has. A subprocess seam asserted against a fake spawner would be asserting
//! the fake - the interesting cases here are interesting because of what the
//! operating system does with a pipe, an exit status, a process group and a
//! kill.
//!
//! What is not restated, and why. Upstream's `pipe` and `inherit` stdio modes,
//! its offset-based non-consuming readers and its spill files serve a
//! protocol consumer this seam has not built: streaming here is a sink that is
//! handed each piece as it arrives (TC-PORT-PROC-16), which is what a caller
//! showing a running command needs, and a second reader of the same stream has
//! no consumer to serve yet. Its credential scrub has nothing to restate
//! because the design is inverted: this child gets what the caller listed, so
//! there is no inherited environment to scrub - TC-PORT-PROC-6 pins that
//! instead. Its remote (E2B) provider is a phase ③ backend.
//!
//! Environmental needs: a POSIX shell at `/bin/sh`, and a writable temp
//! directory. No case reaches a network or an API key. The whole file is
//! skipped off unix rather than silently passing, because a process group is
//! the thing under test and it is a POSIX object. Cases that wait for a
//! timeout use the shortest budget that is not flaky.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tetanus_core::spill::{SpillSource, SpillStore};
use tetanus_exec::proc::{Chunk, Collected, Command, Ending, Limits, ProcessError, Stream};
use tetanus_turn::interrupt::Interrupt;

/// TC-PORT-PROC-1: the two streams are captured, and kept apart.
///
/// Upstream: "captures stdout on success", "captures stderr separately",
/// "captures both streams".
///
/// Keeping them apart matters because a caller shows one to a model and the
/// other to a log, and a tool that interleaved them would make a command's
/// diagnostics indistinguishable from its answer.
///
/// Input: a shell command writing a known line to each stream.
/// Expected: each stream holds its own line and not the other's, neither is
/// marked truncated, and the command reports success.
#[tokio::test]
async fn both_streams_are_captured_and_kept_apart() {
    let output = sh("echo to-stdout; echo to-stderr 1>&2")
        .run()
        .await
        .expect("ran");

    assert_eq!(output.stdout.text, "to-stdout\n");
    assert_eq!(output.stderr.text, "to-stderr\n");
    assert!(!output.stdout.truncated);
    assert!(!output.stderr.truncated);
    assert!(output.ok());
    assert_eq!(output.code, Some(0));
    assert_eq!(output.ending, Ending::Exited);
}

/// TC-PORT-PROC-2: a non-zero exit is reported, and is not an error.
///
/// Upstream: "reports non-zero exit codes".
///
/// A command that ran and failed is a result to read, not a broken request. A
/// caller that got an error here would have nothing to show the model, when
/// what the model needs is precisely the exit code and whatever was printed.
///
/// Input: a command that prints to stderr and exits 3.
/// Expected: `Ok`, carrying code 3, the stderr text, and `ok()` false.
#[tokio::test]
async fn a_non_zero_exit_is_a_result_and_not_an_error() {
    let output = sh("echo nope 1>&2; exit 3").run().await.expect("it ran");

    assert_eq!(output.code, Some(3));
    assert_eq!(output.stderr.text, "nope\n");
    assert!(!output.ok());
    assert!(!output.timed_out());
}

/// TC-PORT-PROC-3: a command that cannot be started is an error, and says
/// which program.
///
/// Upstream: "rejects with a spawn error for a nonexistent cwd".
///
/// This is the other side of TC-PORT-PROC-2's line. Nothing ran, so there is
/// no output and no exit code to report, and pretending otherwise - a
/// synthesised non-zero exit, say - would make "the program is missing"
/// indistinguishable from "the program failed".
///
/// Input: a program that does not exist, and a real program in a working
/// directory that does not exist.
/// Expected: `NotStarted` for both, naming the program.
#[tokio::test]
async fn a_command_that_cannot_start_is_an_error() {
    match Command::new("no-such-program-xyz").run().await {
        Err(ProcessError::NotStarted { program, .. }) => {
            assert_eq!(program, "no-such-program-xyz")
        }
        other => panic!("expected a start failure, got {other:?}"),
    }

    let missing_dir = Command::new("/bin/sh")
        .arg("-c")
        .arg("true")
        .cwd("/definitely/not/here")
        .run()
        .await;
    assert!(matches!(missing_dir, Err(ProcessError::NotStarted { .. })));
}

/// TC-PORT-PROC-4: the command runs where it was told to.
///
/// Upstream: "runs in the requested cwd".
///
/// Input: `pwd` in a temp directory.
/// Expected: the directory that was asked for. Compared against its
/// canonical form, because a temp directory reached through a symlinked
/// `/var` would otherwise fail the comparison on macOS for a reason that has
/// nothing to do with this seam.
#[tokio::test]
async fn a_command_runs_in_the_directory_it_was_given() {
    let dir = TempDir::new().expect("temp dir");
    let canonical = std::fs::canonicalize(dir.path()).expect("canonical");

    let output = sh("pwd").cwd(&canonical).run().await.expect("ran");

    assert_eq!(output.stdout.text.trim(), canonical.display().to_string());
}

/// TC-PORT-PROC-5: output is bounded, and the bound keeps the tail.
///
/// Upstream: "overflow keeps the TAIL" (`SubprocessCollect.maxBytes`).
///
/// Both halves matter. A command that prints without limit must not become
/// resident memory without limit; and when something has to go it is the
/// beginning, because the end of a stream is where the error message and the
/// exit summary are. Keeping the head would reliably discard the only part
/// anyone needed.
///
/// Input: a command printing far more than the bound, with a recognisable
/// last line.
/// Expected: the capture is within the bound, is marked truncated, ends with
/// the last line the command printed, and does not contain the first.
#[tokio::test]
async fn output_is_bounded_and_the_bound_keeps_the_tail() {
    let output =
        sh("echo FIRST-LINE; for i in $(seq 1 4000); do echo padding-$i; done; echo LAST-LINE")
            .limits(Limits {
                max_capture: 2048,
                ..Limits::default()
            })
            .run()
            .await
            .expect("ran");

    assert!(output.stdout.truncated, "it printed more than the bound");
    assert!(
        output.stdout.text.len() <= 2048,
        "kept {} bytes",
        output.stdout.text.len()
    );
    assert!(
        output.stdout.text.trim_end().ends_with("LAST-LINE"),
        "the tail is what was kept: {:?}",
        tail(&output.stdout.text)
    );
    assert!(
        !output.stdout.text.contains("FIRST-LINE"),
        "the head is what was dropped"
    );
}

/// TC-PORT-PROC-6: the child gets what the caller listed, and nothing else.
///
/// Upstream scrubs a denylist out of the inherited environment
/// ("`scrubbedParentEnv`", "merges ordinary extra env entries onto the
/// scrubbed environment"). This inverts that, so there is no scrub to port and
/// this is the case that states why.
///
/// A denylist has to recognise every secret a deployment might have set, and
/// the one it does not recognise is handed to a program a model asked to run.
/// An allowlist fails the other way: a child missing a variable it needed
/// fails loudly and is fixed, which is the way worth failing.
///
/// Input: a distinctive variable set in this process, a child asked to print
/// the environment, and separately a child given one variable explicitly.
/// Expected: the ambient variable is absent from the default child; the listed
/// one is present and is the only thing there; and `inherit_env` does hand the
/// ambient one over, so the escape hatch works and is opt-in.
#[tokio::test]
async fn the_child_gets_what_the_caller_listed_and_nothing_else() {
    // Safety: this test binary sets a variable of its own and reads it back
    // through a child; nothing else in the process depends on this name.
    unsafe { std::env::set_var("TETANUS_PROC_SECRET", "leaked") };

    let bare = sh("env").run().await.expect("ran");
    assert!(
        !bare.stdout.text.contains("TETANUS_PROC_SECRET"),
        "an ambient variable must not reach a child by default: {:?}",
        bare.stdout.text
    );

    let listed = sh("echo \"$WANTED\"")
        .env("WANTED", "given-on-purpose")
        .run()
        .await
        .expect("ran");
    assert_eq!(listed.stdout.text.trim(), "given-on-purpose");

    let inherited = sh("env").inherit_env().run().await.expect("ran");
    assert!(
        inherited.stdout.text.contains("TETANUS_PROC_SECRET"),
        "the opt-in escape hatch hands the environment over"
    );

    unsafe { std::env::remove_var("TETANUS_PROC_SECRET") };
}

/// TC-PORT-PROC-7: standard input is written and closed, and is closed even
/// when there is nothing to write.
///
/// Upstream: "writes stdin to the command and closes it", and "a command that
/// reads stdin sees EOF when none is supplied".
///
/// The second half is the one that hangs if it is wrong. A child reading until
/// end-of-file, with an input nobody closes, waits for its whole timeout and
/// produces nothing - and it looks like the command being slow rather than the
/// harness holding a pipe open.
///
/// Input: a command reading its input, with data supplied and with none.
/// Expected: the data comes back; and with none supplied the command still
/// finishes promptly with empty input, rather than running to its budget.
#[tokio::test]
async fn stdin_is_written_and_is_always_closed() {
    let fed = sh("cat")
        .stdin("hello from the caller")
        .run()
        .await
        .expect("ran");
    assert_eq!(fed.stdout.text, "hello from the caller");
    assert!(fed.ok());

    let empty = sh("cat")
        .limits(Limits {
            timeout: Duration::from_secs(10),
            ..Limits::default()
        })
        .run()
        .await
        .expect("ran");
    assert_eq!(empty.stdout.text, "", "end-of-file, not a wait");
    assert!(empty.ok());
    assert!(
        !empty.timed_out(),
        "an unfed child must not hang on its input"
    );
}

/// TC-PORT-PROC-8: a command that outlives its budget is killed, and says so -
/// and what it printed first still comes back.
///
/// Upstream: "aborts via AbortSignal mid-run", "collects output produced
/// before the kill".
///
/// A timeout is an outcome rather than a failure: the run happened, it was
/// stopped, and the caller needs to be told which. Reporting it as an error
/// would make "this command hangs" indistinguishable from "this command could
/// not be started", which are opposite problems - and throwing away the output
/// would discard the only evidence of how far it got.
///
/// Input: a command that prints a line and then sleeps well past a short
/// budget.
/// Expected: `Ok` with a `TimedOut` ending, no exit code - a killed process has
/// none - `ok()` false, the line it printed before the kill, and a call that
/// returns near the budget rather than near the sleep.
#[tokio::test]
async fn a_command_that_outlives_its_budget_is_killed() {
    let started = std::time::Instant::now();

    let output = sh("echo printed-before-the-kill; sleep 30")
        .limits(brief(Duration::from_millis(300)))
        .run()
        .await
        .expect("a timeout is an outcome, not an error");

    assert_eq!(output.ending, Ending::TimedOut);
    assert!(output.timed_out());
    assert_eq!(output.code, None, "a killed process reports no exit code");
    assert!(!output.ok());
    assert_eq!(
        output.stdout.text.trim(),
        "printed-before-the-kill",
        "what it printed before the kill is the useful part"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "it waited {:?}, which is the sleep and not the budget",
        started.elapsed()
    );
}

/// TC-PORT-PROC-9: arguments are passed as given, and never through a shell.
///
/// Not an upstream case in this shape - upstream's seam is argv-only by
/// construction - but it is the property that decides whether this primitive
/// is safe to build a tool on. If an argument were concatenated into a shell
/// line, every caller would inherit a quoting problem, and the first
/// model-supplied filename with a space or a semicolon in it would run
/// something nobody wrote.
///
/// Input: arguments containing spaces, quotes, a semicolon, a `$`, and a
/// backtick, echoed back one per line.
/// Expected: each argument arrives exactly as written, as one argument.
#[tokio::test]
async fn arguments_are_passed_as_given_and_never_through_a_shell() {
    let hostile = [
        "plain",
        "with spaces",
        "; rm -rf /",
        "$(whoami)",
        "`id`",
        "quote\"and'quote",
    ];

    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"for a in "$@"; do echo "[$a]"; done"#)
        .arg("sh")
        .args(hostile)
        .run()
        .await
        .expect("ran");

    let seen: Vec<String> = output
        .stdout
        .text
        .lines()
        .map(|line| line.trim_matches(|c| c == '[' || c == ']').to_string())
        .collect();
    assert_eq!(seen, hostile, "each argument survived exactly");
}

/// TC-PORT-PROC-10: a truncated capture is still valid text.
///
/// A tail cut at a byte bound lands in the middle of a character routinely -
/// the bound comes from a budget, not from the text - and a capture that
/// handed back a broken string would push that hazard onto every caller,
/// including the one that puts it in a JSON tool result.
///
/// Input: a command printing many multi-byte characters, under a bound chosen
/// so the cut falls inside one.
/// Expected: the capture is truncated, is valid text, and does not begin with
/// a replacement character - the partial character at the cut is dropped
/// rather than rendered as a glyph the command never printed.
#[tokio::test]
async fn a_truncated_capture_is_still_valid_text() {
    for bound in [1001, 1002, 1003, 1004] {
        let output = sh(&format!(
            "for i in $(seq 1 2000); do printf '{MULTIBYTE}'; done"
        ))
        .limits(Limits {
            max_capture: bound,
            ..Limits::default()
        })
        .run()
        .await
        .expect("ran");

        assert!(output.stdout.truncated, "bound {bound} should truncate");
        assert!(
            !output.stdout.text.starts_with('\u{FFFD}'),
            "bound {bound} left a half character at the cut: {:?}",
            output.stdout.text.chars().take(4).collect::<String>()
        );
        // The string type already guarantees validity; this asserts the bytes
        // behind it round-trip, so a lossy conversion that invented a
        // character would show up.
        assert!(
            output.stdout.text.chars().all(|c| c != '\u{FFFD}'),
            "bound {bound} rendered a character the command never printed"
        );
    }
}

/// TC-PORT-PROC-11: the kill reaches the grandchildren, not just the child.
///
/// Upstream: "terminate kills the whole process tree", "grandchildren do not
/// survive termination".
///
/// This is the case the previous seam could not pass, and it is the whole
/// reason a command is spawned as its own process-group leader. A model asks
/// for `npm test`; npm starts a test runner; the runner starts a server. Kill
/// the shell alone and the server keeps the port, keeps the CPU, and keeps the
/// pipe - and the next command fails for a reason nobody can see.
///
/// Input: a shell that records a grandchild's pid and then sleeps past a short
/// budget.
/// Expected: the run times out, and the grandchild is gone afterwards - not
/// merely orphaned.
#[tokio::test]
async fn the_kill_reaches_the_grandchildren() {
    let dir = TempDir::new().expect("temp dir");
    let pidfile = dir.path().join("grandchild.pid");

    let output = sh(&format!(
        "sleep 30 & echo $! > {}; sleep 30",
        pidfile.display()
    ))
    .limits(brief(Duration::from_millis(400)))
    .run()
    .await
    .expect("a timeout is an outcome");

    assert!(output.timed_out());
    let grandchild = read_pid(&pidfile);
    assert!(
        !alive(grandchild),
        "grandchild {grandchild} outlived the kill that ended its parent"
    );
}

/// TC-PORT-PROC-12: a command that ignores SIGTERM is still ended.
///
/// Upstream: "escalates SIGTERM to SIGKILL after the grace period".
///
/// The polite rung exists so a script can clean up; the second rung exists
/// because politeness is a request. A ladder with only its first rung is a
/// timeout that does not time anything out, which is worse than no timeout at
/// all: the caller believes the budget is enforced.
///
/// Input: a shell that ignores SIGTERM and loops, under a short budget and a
/// short grace.
/// Expected: it ends, killed by SIGKILL, and the call takes at least the grace
/// (so the polite rung was really tried) and nothing like the loop.
#[tokio::test]
async fn a_command_that_ignores_sigterm_is_still_ended() {
    let started = std::time::Instant::now();

    let output = sh("trap '' TERM; while true; do sleep 0.05; done")
        .limits(Limits {
            timeout: Duration::from_millis(200),
            grace: Duration::from_millis(250),
            ..Limits::default()
        })
        .run()
        .await
        .expect("a timeout is an outcome");

    assert!(output.timed_out());
    assert_eq!(
        output.signal.as_deref(),
        Some("SIGKILL"),
        "the polite rung was ignored, so the second one had to land"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "SIGKILL came before the grace expired: {:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the loop outlived the ladder: {:?}",
        started.elapsed()
    );
}

/// TC-PORT-PROC-13: the signal that ended a command is named.
///
/// Upstream: "reports the terminating signal" (`SubprocessOutcome.signal`).
///
/// A caller renders `[killed by signal: SIGKILL]` and a model reads it. Exit
/// code `None` alone says only "no code", which a reader cannot tell from a
/// harness that forgot to collect one.
///
/// Input: a command that kills itself with SIGKILL.
/// Expected: no exit code, the signal named, and the ending still `Exited` -
/// the command chose this, the harness did not.
#[tokio::test]
async fn the_signal_that_ended_a_command_is_named() {
    let output = sh("kill -KILL $$").run().await.expect("ran");

    assert_eq!(output.code, None);
    assert_eq!(output.signal.as_deref(), Some("SIGKILL"));
    assert_eq!(output.ending, Ending::Exited);
    assert!(!output.ok());
}

/// TC-PORT-PROC-14: an orphan holding the pipe does not hold the call open.
///
/// Upstream reaches the same place from the other side: its collected reads
/// end at process close, and its disposal terminates the managed tree. The
/// hazard restated here is the one a pipe creates - a background process
/// inherits stdout, so the *leader* exiting does not close the stream, and a
/// reader waiting for end-of-file waits for the orphan instead of the command.
///
/// Input: `sleep 30 & echo done` - the leader exits immediately, its
/// grandchild keeps the pipe.
/// Expected: the call returns promptly with the command's output, says the
/// group had to be swept, and the orphan is gone.
#[tokio::test]
async fn an_orphan_holding_the_pipe_does_not_hold_the_call_open() {
    let dir = TempDir::new().expect("temp dir");
    let pidfile = dir.path().join("orphan.pid");
    let started = std::time::Instant::now();

    let output = sh(&format!(
        "sleep 30 & echo $! > {}; echo done",
        pidfile.display()
    ))
    .limits(Limits {
        timeout: Duration::from_secs(30),
        grace: Duration::from_millis(300),
        ..Limits::default()
    })
    .run()
    .await
    .expect("ran");

    assert_eq!(output.code, Some(0), "the command itself succeeded");
    assert!(output.stdout.text.contains("done"));
    assert!(
        output.swept,
        "something was still holding the pipe, and the caller is told so"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the call waited for the orphan: {:?}",
        started.elapsed()
    );
    let orphan = read_pid(&pidfile);
    assert!(!alive(orphan), "orphan {orphan} survived the sweep");
}

/// TC-PORT-PROC-15: a command that closes its streams tidily is not swept.
///
/// The other half of TC-PORT-PROC-14: the sweep must be the exception. A seam
/// that reported every command as swept would make the flag useless and would
/// be signalling a group nobody is in.
///
/// Input: an ordinary command.
/// Expected: it succeeds and `swept` is false.
#[tokio::test]
async fn an_ordinary_command_is_not_swept() {
    let output = sh("echo tidy").run().await.expect("ran");

    assert!(output.ok());
    assert!(!output.swept, "nothing was left to kill");
}

/// TC-PORT-PROC-16: a long-running command is readable before it ends.
///
/// Upstream: "streams output incrementally", "readers observe output before
/// close".
///
/// Buffering to completion is the difference between a caller that can show
/// progress and a caller that shows a spinner for ten minutes. The assertion
/// is a timing one on purpose: a sink that received everything at the end
/// would satisfy any content-only check.
///
/// Input: a command that prints, sleeps, prints again, with a sink attached
/// and a watcher reading the sink while the command is still running.
/// Expected: the watcher sees the first line before the call returns - the
/// claim stated as an ordering rather than as a stopwatch reading, so a loaded
/// machine cannot turn "it streamed" into "it was slow"; both streams reach the
/// sink tagged with which they came from; and the captured tails still hold
/// everything.
#[tokio::test]
async fn a_long_running_command_is_readable_before_it_ends() {
    let sink = Arc::new(Timed::default());
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let sink = Arc::clone(&sink);
        let finished = Arc::clone(&finished);
        tokio::spawn(async move {
            loop {
                if sink.text().contains("first") {
                    return !finished.load(std::sync::atomic::Ordering::Acquire);
                }
                if finished.load(std::sync::atomic::Ordering::Acquire) {
                    return false;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };
    let started = std::time::Instant::now();

    let output = sh("echo first; echo to-err 1>&2; sleep 1; echo second")
        .streaming(sink.clone())
        .run()
        .await
        .expect("ran");
    finished.store(true, std::sync::atomic::Ordering::Release);

    let elapsed = started.elapsed();
    assert!(
        watcher.await.expect("the watcher ran"),
        "nothing reached the sink until the command had ended, which is buffering"
    );
    let first = sink.first_at().expect("something reached the sink");
    assert!(
        first < elapsed,
        "the first chunk arrived at {first:?} of a {elapsed:?} command"
    );
    let text = sink.text();
    assert!(
        text.contains("first") && text.contains("second"),
        "{text:?}"
    );
    assert!(
        sink.chunks()
            .iter()
            .any(|chunk| chunk.stream == Stream::Stderr && chunk.text.contains("to-err")),
        "the sink is told which stream each piece came from"
    );
    assert!(
        output.stdout.text.contains("second"),
        "the tail is still captured"
    );
    assert!(output.stderr.text.contains("to-err"));
}

/// TC-PORT-PROC-17: an interrupt ends the command and its group.
///
/// Upstream: "aborts via AbortSignal mid-run"; here the abort is the turn's
/// own interrupt, which is the only cancellation this workspace has.
///
/// A turn the user stopped must not leave a command running: nobody will read
/// its answer, and on this seam it would go on holding a pipe and a process
/// group after the turn that asked for it is closed.
///
/// Input: a sleeping command with a distant budget, interrupted after it has
/// started.
/// Expected: the call returns promptly, says it was interrupted rather than
/// timed out, and the child is gone.
#[tokio::test]
async fn an_interrupt_ends_the_command_and_its_group() {
    let dir = TempDir::new().expect("temp dir");
    let pidfile = dir.path().join("child.pid");
    let interrupt = Interrupt::new();
    let stopper = Arc::clone(&interrupt);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        stopper.stop();
    });

    let started = std::time::Instant::now();
    let output = sh(&format!(
        "sleep 30 & echo $! > {}; sleep 30",
        pidfile.display()
    ))
    .limits(Limits {
        timeout: Duration::from_secs(60),
        grace: Duration::from_millis(200),
        ..Limits::default()
    })
    .run_watching(&interrupt)
    .await
    .expect("an interrupt is an outcome, not an error");

    assert_eq!(output.ending, Ending::Interrupted);
    assert!(output.interrupted() && !output.timed_out());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the interrupt did not stop it: {:?}",
        started.elapsed()
    );
    let child = read_pid(&pidfile);
    assert!(!alive(child), "child {child} survived the interrupt");
}

/// TC-PORT-PROC-19: what the bound drops is kept, and the result says where.
///
/// Upstream: `subprocess-local` writes the complete stream to a file when its
/// in-memory bound drops bytes, and reports the path.
///
/// This can only be done by the producer, which is the whole reason it lives
/// here. A bounded capture drops its beginning *while the command runs*, so by
/// the time a result exists the bytes are gone and no policy above this seam
/// can spill what it never saw - `tetanus_core::spill` applied to a finished
/// result would file the tail and call it the output.
///
/// Input: a command printing far more than the bound keeps, with a spill
/// store.
/// Expected: the capture is bounded and says it was cut; the artifact holds
/// every line including the first, which the capture no longer has; and the
/// result names where it is.
#[tokio::test]
async fn what_the_bound_drops_is_kept_and_the_result_says_where() {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(SpillStore::at(dir.path().join("spill")));

    let output = sh("for i in $(seq 1 20000); do echo line-$i; done")
        .limits(Limits {
            max_capture: 4 * 1024,
            ..Limits::default()
        })
        .spilling(Arc::clone(&store), source("run-1"))
        .run()
        .await
        .expect("the command ran");

    assert!(
        output.stdout.truncated,
        "the bound should have dropped some"
    );
    assert!(
        !output.stdout.text.contains("line-1\n"),
        "the capture keeps the tail, so the first line is gone from it"
    );
    let locator = output
        .stdout
        .spilled
        .as_ref()
        .expect("the whole stream was kept");
    let whole = std::fs::read_to_string(locator).expect("the artifact is readable");
    assert!(
        whole.contains("line-1\n") && whole.contains("line-20000\n"),
        "the artifact is the whole stream, not the part that arrived after the bound was hit"
    );
    assert_eq!(
        whole
            .lines()
            .filter(|line| line.starts_with("line-"))
            .count(),
        20_000,
        "every line is there exactly once"
    );
}

/// TC-PORT-PROC-20: a command that fits costs no artifact, and the two streams
/// are told apart.
///
/// The other half of TC-PORT-PROC-19, and the reason the file is opened on the
/// first overflow rather than at the start: a harness that filed every command
/// would fill a disk with the output of `echo`. The stream suffix matters for
/// the opposite reason - a command that overran on both produces two
/// artifacts, and "which of these is stderr" should not need reading them.
///
/// Input: a command whose output fits the bound, then one that overruns both
/// streams.
/// Expected: nothing is spilled for the first and no directory is even made;
/// the second names two different artifacts, one per stream, each holding that
/// stream's output.
#[tokio::test]
async fn a_command_that_fits_costs_no_artifact_and_the_streams_are_told_apart() {
    let dir = TempDir::new().expect("temp dir");
    let root = dir.path().join("spill");
    let store = Arc::new(SpillStore::at(&root));

    let small = sh("echo just a line")
        .spilling(Arc::clone(&store), source("run-1"))
        .run()
        .await
        .expect("the command ran");
    assert_eq!(small.stdout.spilled, None, "nothing was dropped");
    assert!(
        !root.exists(),
        "a command that fits should not make a spill directory at all"
    );

    let both = sh("for i in $(seq 1 4000); do echo out-$i; echo err-$i 1>&2; done")
        .limits(Limits {
            max_capture: 2 * 1024,
            ..Limits::default()
        })
        .spilling(Arc::clone(&store), source("run-2"))
        .run()
        .await
        .expect("the command ran");

    let out = both.stdout.spilled.expect("stdout was spilled");
    let err = both.stderr.spilled.expect("stderr was spilled");
    assert_ne!(out, err, "one artifact per stream");
    assert!(
        out.contains("stdout") && err.contains("stderr"),
        "the artifacts name their stream: {out} / {err}"
    );
    let out = std::fs::read_to_string(&out).expect("readable");
    let err = std::fs::read_to_string(&err).expect("readable");
    assert!(out.contains("out-1\n") && !out.contains("err-1\n"));
    assert!(err.contains("err-1\n") && !err.contains("out-1\n"));
}

/// TC-PORT-PROC-21: a spill that cannot be written is not a failed command.
///
/// `tetanus_core::spill` states the rule for tool results and it is the same
/// rule here: storage that is full, absent or refused must leave the command
/// exactly as it would have been. A harness that turned a successful build
/// into an error because it could not file the log away has broken the thing
/// the model was actually doing.
///
/// Input: a command that overruns its bound, with a spill root that cannot be
/// created because a file is in its way.
/// Expected: the command still reports its exit status and its bounded tail,
/// and simply names no artifact.
#[tokio::test]
async fn a_spill_that_cannot_be_written_is_not_a_failed_command() {
    let dir = TempDir::new().expect("temp dir");
    let blocked = dir.path().join("not-a-directory");
    std::fs::write(&blocked, "a file where a directory would go").expect("wrote the blocker");
    let store = Arc::new(SpillStore::at(&blocked));

    let output = sh("for i in $(seq 1 5000); do echo line-$i; done; echo THE-END")
        .limits(Limits {
            max_capture: 2 * 1024,
            ..Limits::default()
        })
        .spilling(store, source("run-1"))
        .run()
        .await
        .expect("the command ran");

    assert_eq!(output.code, Some(0), "the command itself was fine");
    assert!(output.stdout.truncated);
    assert_eq!(output.stdout.spilled, None, "nothing is promised");
    assert!(
        output.stdout.text.contains("THE-END"),
        "the bounded tail is still there: {}",
        tail(&output.stdout.text)
    );
}

/// TC-PORT-PROC-18: a character split across two reads is delivered whole.
///
/// A chunk boundary falls wherever the pipe happened to fill, so a multi-byte
/// character is routinely split between two reads. A sink handed the halves
/// would show a replacement glyph the command never printed, and a consumer
/// concatenating chunks would never recover the character.
///
/// Input: a command printing many multi-byte characters, read through a sink.
/// Expected: no chunk carries a replacement character, and the concatenation
/// of every chunk is exactly what the command printed.
#[tokio::test]
async fn a_character_split_across_two_reads_is_delivered_whole() {
    let sink = Arc::new(Collected::new());
    let count = 20_000;

    let output = sh(&format!(
        "for i in $(seq 1 {count}); do printf '{MULTIBYTE}'; done"
    ))
    .streaming(sink.clone())
    .limits(Limits {
        max_capture: 1024 * 1024,
        ..Limits::default()
    })
    .run()
    .await
    .expect("ran");

    let streamed = sink.text();
    assert!(
        !streamed.contains('\u{FFFD}'),
        "a chunk boundary produced a character the command never printed"
    );
    assert_eq!(
        streamed.chars().count(),
        count * 2,
        "every character printed reached the sink exactly once"
    );
    assert!(
        sink.chunks().len() > 1,
        "one chunk is not a boundary; this case needs the stream split"
    );
    assert_eq!(
        streamed, output.stdout.text,
        "the stream and the capture are the same bytes"
    );
}

/// A sink that remembers when its first chunk arrived, which is what makes
/// TC-PORT-PROC-16 a statement about streaming rather than about content.
#[derive(Default)]
struct Timed {
    inner: Collected,
    first: std::sync::Mutex<Option<std::time::Instant>>,
    born: Once,
}

/// The instant the sink was made, resolved lazily so `Default` stays simple.
struct Once(std::sync::OnceLock<std::time::Instant>);

impl Default for Once {
    fn default() -> Self {
        let cell = std::sync::OnceLock::new();
        let _ = cell.set(std::time::Instant::now());
        Self(cell)
    }
}

impl Timed {
    fn first_at(&self) -> Option<Duration> {
        let born = *self.born.0.get().expect("set at construction");
        self.first
            .lock()
            .expect("no panic holds this lock")
            .map(|at| at.duration_since(born))
    }

    fn text(&self) -> String {
        self.inner.text()
    }

    fn chunks(&self) -> Vec<Chunk> {
        self.inner.chunks()
    }
}

impl tetanus_exec::proc::OutputSink for Timed {
    fn chunk(&self, chunk: Chunk) {
        self.first
            .lock()
            .expect("no panic holds this lock")
            .get_or_insert_with(std::time::Instant::now);
        self.inner.chunk(chunk);
    }
}

/// `é中`, written as the octal escapes POSIX `printf` defines - five bytes and
/// two characters, so a cut or a chunk boundary inside one is reachable. A
/// `\u` escape is not POSIX and the shell behind `/bin/sh` on a Debian host
/// prints it literally, which would make a multi-byte case pass on ASCII.
const MULTIBYTE: &str = r"\303\251\344\270\255";

/// A `/bin/sh -c` command, which is how every case here says what to run.
fn sh(script: &str) -> Command {
    Command::new("/bin/sh").arg("-c").arg(script)
}

/// A short budget with a short grace, so a case that waits for the ladder
/// waits for milliseconds rather than for the default three seconds.
fn brief(timeout: Duration) -> Limits {
    Limits {
        timeout,
        grace: Duration::from_millis(200),
        ..Limits::default()
    }
}

/// The pid a case's shell recorded, waiting briefly for the file to appear:
/// the shell writes it as it starts, and the assertion is about what happened
/// afterwards.
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

/// One spill source, for a case that only cares that two runs differ.
fn source(call_id: &str) -> SpillSource {
    SpillSource {
        session_id: "a-session".to_string(),
        tool: "shell".to_string(),
        call_id: call_id.to_string(),
    }
}

/// The last of a capture, for a failure message that is readable.
fn tail(text: &str) -> String {
    text.chars()
        .rev()
        .take(60)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}
