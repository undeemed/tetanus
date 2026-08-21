//! Test Design Specification: running one external command, ported.
//!
//! Feature under test: `tetanus_turn::process::Command` - the primitive under
//! everything that has to leave the process. Upstream pins the same
//! collected-output behaviour in
//! `packages/subprocess/subprocess-local/tests/spawn.spec.ts`; each case names
//! the upstream case it comes from.
//!
//! Approach: real processes, via `/bin/sh` and the coreutils every POSIX host
//! has. A subprocess seam asserted against a fake spawner would be asserting
//! the fake - the interesting cases here are interesting because of what the
//! operating system does with a pipe, an exit status and a kill.
//!
//! What is not restated, and why. Upstream terminates a whole process group
//! with a SIGTERM-to-SIGKILL escalation, and several of its cases are about
//! that ladder - a trapped SIGTERM, grandchildren dying, a group of zombies,
//! an idempotent second terminate. This seam kills the child it started and
//! nothing else, because a process-group call needs a platform dependency this
//! workspace does not have; the gap is stated in the module and carried in
//! `docs/parity.md` rather than papered over with a case that passes for the
//! wrong reason. Upstream's `pipe` and `inherit` stdio modes, its offset-based
//! non-consuming readers and its spill files serve a streaming consumer this
//! has not built. Its credential scrub has nothing to restate because the
//! design is inverted: this child gets what the caller listed, so there is no
//! inherited environment to scrub - TC-PORT-PROC-6 pins that instead.
//!
//! Environmental needs: a POSIX shell at `/bin/sh`, and a writable temp
//! directory. No case reaches a network or an API key. The whole file is
//! skipped off unix rather than silently passing. One case waits for a
//! timeout, and uses the shortest budget that is not flaky.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(unix)]

use std::time::Duration;

use tempfile::TempDir;
use tetanus_turn::process::{Command, Limits, ProcessError};

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
    assert!(!output.timed_out);
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
        !empty.timed_out,
        "an unfed child must not hang on its input"
    );
}

/// TC-PORT-PROC-8: a command that outlives its budget is killed, and says so.
///
/// Upstream: "aborts via AbortSignal mid-run".
///
/// A timeout is an outcome rather than a failure: the run happened, it was
/// stopped, and the caller needs to be told which. Reporting it as an error
/// would make "this command hangs" indistinguishable from "this command could
/// not be started", which are opposite problems.
///
/// Input: a command that sleeps well past a short budget.
/// Expected: `Ok` with `timed_out` true, no exit code - a killed process has
/// none - `ok()` false, and the call returns near the budget rather than near
/// the sleep.
#[tokio::test]
async fn a_command_that_outlives_its_budget_is_killed() {
    let started = std::time::Instant::now();

    let output = sh("sleep 30")
        .limits(Limits {
            timeout: Duration::from_millis(300),
            ..Limits::default()
        })
        .run()
        .await
        .expect("a timeout is an outcome, not an error");

    assert!(output.timed_out);
    assert_eq!(output.code, None, "a killed process reports no exit code");
    assert!(!output.ok());
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
        let output = sh("for i in $(seq 1 2000); do printf '\\u00e9\\u4e2d'; done")
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

/// A `/bin/sh -c` command, which is how every case here says what to run.
fn sh(script: &str) -> Command {
    Command::new("/bin/sh").arg("-c").arg(script)
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
