//! Test Design Specification: the shell backends and the one-shot executor
//! over them, ported.
//!
//! Features under test: `tetanus_exec::backend` - which shell a command runs
//! through, how it is asked, and what happens when its binary is not on this
//! host - and `tetanus_exec::shell` - the deployment's defaults and caps, the
//! run itself, and the text a model reads afterwards. Upstream pins the same
//! decisions in `packages/shell/bash-local/tests/executor.spec.ts`,
//! `packages/shell/pwsh-local/tests/executor.spec.ts`,
//! `packages/shell/shell/tests/render.spec.ts` and
//! `packages/shell/tool-bash/tests/tools.spec.ts`.
//!
//! Approach: a real bash, and a real absence of pwsh. The refusal cases are
//! the point of the trait, so they are asserted against a host that genuinely
//! lacks the backend rather than against a stub that pretends to.
//!
//! What is not restated, and why. Upstream's sandbox families
//! (`bash-sandbox`, `pwsh-sandbox`, and the escalation prose in its tool) are
//! phase ③ here: `docs/parity.md` carries the sandbox row separately and this
//! seam confines nothing. Its `shell-env` package collects `DSH_*` facts about
//! a running session into the child environment; tetanus has no header
//! metadata to collect yet (the same gap the `core/*` row names), so the
//! environment half restates as "what the caller listed wins over the
//! backend's defaults". Its background/`jobs` half needs a job store this
//! phase has not built. Its settings section is a document key, and belongs
//! with the other engine settings rather than here.
//!
//! Environmental needs: a bash on PATH, and a writable temp directory. No case
//! reaches a network or an API key. The pwsh cases run on a host without
//! PowerShell and say so in their expectations; on a host that has one, the
//! absence case reports itself skipped rather than failing for the wrong
//! reason.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use tetanus_exec::backend::{BackendError, Bash, Markers, PowerShell, ShellBackend};
use tetanus_exec::proc::Ending;
use tetanus_exec::shell::{parse_exit, render, ShellConfig, ShellError, ShellExec, ShellRequest};

/// TC-PORT-SHELL-1: the bash backend resolves to a program on this host.
///
/// Upstream: `resolveExecutable` ("resolves a bare name through PATH",
/// "verifies an absolute path").
///
/// Resolution happens once, at composition, so a deployment whose shell is
/// missing finds out before a model asks for a command rather than in the
/// middle of a turn.
///
/// Input: the default bash backend, and one pinned to an explicit path.
/// Expected: both resolve; the resolved program exists and is named `bash`
/// for the searched one, and is exactly what was written for the pinned one.
#[test]
fn the_bash_backend_resolves_to_a_program_on_this_host() {
    let resolved = Bash::new().resolve().expect("this host has a bash");
    assert_eq!(resolved.backend(), "bash");
    assert!(
        resolved.program().is_absolute(),
        "a bare name resolves to where it was found: {}",
        resolved.program().display()
    );
    assert!(resolved.program().exists());

    let pinned = Bash::at("/bin/sh").resolve().expect("an explicit path");
    assert_eq!(pinned.program().display().to_string(), "/bin/sh");
}

/// TC-PORT-SHELL-2: a backend whose binary is absent refuses, and names what
/// is missing.
///
/// Upstream: "rejects when the executable cannot be resolved".
///
/// The refusal is the behaviour, and the *loudness* is the design decision.
/// Falling back to another shell would run a bash script under dash, and the
/// first `[[` or `pipefail` would fail with a syntax error a user would spend
/// an afternoon attributing to their own command instead of to a deployment
/// that never had bash.
///
/// Input: a backend pinned to a program that does not exist, and PowerShell on
/// a host without it.
/// Expected: `Missing`, naming the program and where it looked; and the
/// message says nothing was substituted. Nothing is spawned.
#[test]
fn a_backend_whose_binary_is_absent_refuses_loudly() {
    let refused = Bash::at("/nowhere/bin/bash")
        .resolve()
        .expect_err("that program is not there");
    // One variant today, so this binds rather than matches; a second variant
    // would make this line a compile error, which is the right place to be
    // told a refusal grew a new shape.
    let BackendError::Missing {
        backend,
        program,
        looked_in,
    } = &refused;
    assert_eq!(*backend, "bash");
    assert_eq!(program, "/nowhere/bin/bash");
    assert_eq!(looked_in.len(), 1, "an explicit path is looked for once");
    let message = refused.to_string();
    assert!(
        message.contains("no other shell was substituted"),
        "the refusal has to rule out the silent fallback: {message}"
    );

    if PowerShell::new().resolve().is_ok() {
        eprintln!("skipped: this host has a PowerShell, so its absence cannot be observed");
        return;
    }
    let absent = PowerShell::new()
        .resolve()
        .expect_err("no PowerShell on a POSIX CI host");
    let BackendError::Missing {
        backend, looked_in, ..
    } = &absent;
    assert_eq!(*backend, "pwsh");
    assert!(
        looked_in.iter().any(|path| path.ends_with("pwsh")),
        "it says where it looked: {looked_in:?}"
    );
}

/// TC-PORT-SHELL-3: both shells are the same trait, and each is asked its own
/// way.
///
/// Upstream keeps two executor packages with one seam between them
/// (`ShellExecutor`), and the win32 layer swaps one for the other.
///
/// A seam with one hard-coded `bash -c` in it does not have a Windows gap; it
/// has a Windows rewrite. This case is what makes the difference checkable:
/// the PowerShell backend is a value on this host even though its binary is
/// not, and it describes an invocation nobody has to guess at later.
///
/// Input: both backends, asked for their one-shot argv, their session argv and
/// their marker wrapper.
/// Expected: bash asks with `-c` and one argument; pwsh asks with
/// `-NoProfile -NonInteractive -Command` and prefixes the UTF-8 preamble; each
/// wrapper is one physical line carrying both markers.
#[test]
fn both_shells_are_the_same_trait_asked_their_own_way() {
    let bash = Bash::new();
    let pwsh = PowerShell::new();

    assert_eq!(bash.one_shot("echo hi"), vec!["-c", "echo hi"]);
    let asked = pwsh.one_shot("echo hi");
    assert_eq!(
        asked[..4],
        ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
    );
    assert!(
        asked[4].ends_with("echo hi") && asked[4].contains("UTF8Encoding"),
        "the encoding preamble runs before the command: {:?}",
        asked[4]
    );
    assert_eq!(asked.len(), 5, "the command stays one argument");

    assert!(bash.session().contains(&"--norc".to_string()));
    assert!(pwsh.session().contains(&"-NonInteractive".to_string()));

    let markers = Markers::new("nonce");
    for backend in [&bash as &dyn ShellBackend, &pwsh as &dyn ShellBackend] {
        let wrapped = backend.wrap("ls\nls", &markers);
        assert!(
            !wrapped.contains('\n'),
            "{} wrapped a command across lines, which a session would run as two: {wrapped:?}",
            backend.name()
        );
        assert!(
            wrapped.contains(&markers.start) && wrapped.contains(&markers.end),
            "{} lost a marker",
            backend.name()
        );
    }
}

/// TC-PORT-SHELL-4: a command runs, and its result is a result.
///
/// Upstream: "runs a command and collects stdout", "reports a non-zero exit
/// without rejecting".
///
/// Input: a successful command and a failing one, through the bash executor.
/// Expected: the output comes back; the failing one has its code and its
/// stderr and is not an error.
#[tokio::test]
async fn a_command_runs_and_its_result_is_a_result() {
    let exec = bash_exec(ShellConfig::default());

    let ran = run(&exec, ShellRequest::new("echo out; echo err 1>&2")).await;
    assert_eq!(ran.output.stdout.text, "out\n");
    assert_eq!(ran.output.stderr.text, "err\n");
    assert!(ran.output.ok());

    let failed = run(&exec, ShellRequest::new("exit 7")).await;
    assert_eq!(failed.output.code, Some(7));
    assert!(!failed.output.ok());
    assert_eq!(failed.output.ending, Ending::Exited);
}

/// TC-PORT-SHELL-5: the deployment's defaults are filled in, and its caps are
/// applied.
///
/// Upstream: `resolve()` ("applies the configured default timeout", "caps a
/// larger requested timeout", "defaults workdir to the configured cwd").
///
/// Resolution is a separate step so a caller cannot route around the caps by
/// running a raw request, and so the number a timeout marker quotes is the
/// number that actually applied.
///
/// Input: a request with nothing set, one asking for more than the cap, one
/// asking for less, and one naming a relative directory.
/// Expected: the default timeout; the cap; the smaller value untouched; and a
/// relative directory resolved against the deployment's own.
#[test]
fn the_deployments_defaults_are_filled_in_and_its_caps_applied() {
    let dir = tempfile::tempdir().expect("temp dir");
    let exec = bash_exec(ShellConfig {
        cwd: dir.path().to_path_buf(),
        timeout: Duration::from_millis(500),
        max_timeout: Duration::from_secs(2),
        ..ShellConfig::default()
    });

    let bare = exec.resolve(ShellRequest::new("true")).expect("resolved");
    assert_eq!(bare.timeout, Duration::from_millis(500));
    assert_eq!(bare.workdir, dir.path());

    let greedy = exec
        .resolve(ShellRequest::new("true").timeout(Duration::from_secs(86_400)))
        .expect("resolved");
    assert_eq!(greedy.timeout, Duration::from_secs(2), "the cap applied");

    let modest = exec
        .resolve(ShellRequest::new("true").timeout(Duration::from_millis(50)))
        .expect("resolved");
    assert_eq!(modest.timeout, Duration::from_millis(50));

    let relative = exec
        .resolve(ShellRequest::new("true").workdir("inner"))
        .expect("resolved");
    assert_eq!(relative.workdir, dir.path().join("inner"));

    let empty = exec.resolve(ShellRequest::new("   "));
    assert!(matches!(empty, Err(ShellError::EmptyCommand)));
}

/// TC-PORT-SHELL-6: the backend's environment is a default, and the caller's
/// entry wins.
///
/// Upstream: "merges ordinary extra env entries onto the scrubbed
/// environment", with `ENV_OVERRIDES` merged first so a trusted caller's own
/// entry still wins.
///
/// The overrides exist so a model does not read ANSI escapes and a pager does
/// not wait for a keypress that will never come. They are not policy: a caller
/// that sets `TERM` itself has a reason this list cannot know.
///
/// Input: a command printing three variables, once bare and once with `TERM`
/// named by the caller.
/// Expected: the overrides are there by default; the caller's `TERM` replaces
/// the default one; and the ambient environment is still absent, because the
/// seam below hands over only what was listed.
#[tokio::test]
async fn the_backends_environment_is_a_default_the_caller_can_beat() {
    // Safety: this test binary sets a variable of its own and reads it back
    // through a child; nothing else in the process depends on this name.
    unsafe { std::env::set_var("TETANUS_SHELL_SECRET", "leaked") };
    let exec = bash_exec(ShellConfig::default());

    let bare = run(
        &exec,
        ShellRequest::new("echo \"$TERM|$NO_COLOR|$PAGER|$TETANUS_SHELL_SECRET\""),
    )
    .await;
    assert_eq!(bare.output.stdout.text.trim(), "dumb|1|cat|");

    let named = run(
        &exec,
        ShellRequest::new("echo \"$TERM\"").env("TERM", "xterm-256color"),
    )
    .await;
    assert_eq!(named.output.stdout.text.trim(), "xterm-256color");

    unsafe { std::env::remove_var("TETANUS_SHELL_SECRET") };
}

/// TC-PORT-SHELL-7: a command that hangs is killed by its budget, and what it
/// printed first survives.
///
/// Upstream: "kills the command when the timeout expires", "reports timedOut".
///
/// Input: a command that prints and then sleeps far past a short budget.
/// Expected: a `TimedOut` ending, the early output, the effective budget on
/// the result, and a rendered text that says so.
#[tokio::test]
async fn a_command_that_hangs_is_killed_by_its_budget() {
    let exec = bash_exec(ShellConfig {
        timeout: Duration::from_millis(300),
        grace: Duration::from_millis(200),
        ..ShellConfig::default()
    });

    let ran = run(&exec, ShellRequest::new("echo working; sleep 30")).await;

    assert_eq!(ran.output.ending, Ending::TimedOut);
    assert!(ran.output.stdout.text.contains("working"));
    assert_eq!(ran.timeout, Duration::from_millis(300));
    let text = render(&ran);
    assert!(
        text.contains("[timed out after 300ms]"),
        "the marker quotes the budget that applied: {text}"
    );
}

/// TC-PORT-SHELL-8: the rendered result is what upstream's markers say.
///
/// Upstream: `render.spec.ts` and `tool-bash/tests/tools.spec.ts` ("renders
/// stdout then a marked stderr section", "appends [exit code: N]", "reports a
/// signal kill", "(no output) for a silent command").
///
/// This is a wire format in all but name: a presentation parses it back to
/// show an exit pill, so the shape is not free to drift.
///
/// Input: four runs - clean, failing, silent, and signal-killed.
/// Expected: the exact markers, exit last, and `(no output)` for a command
/// that printed nothing.
#[tokio::test]
async fn the_rendered_result_is_what_the_markers_say() {
    let exec = bash_exec(ShellConfig::default());

    let clean = render(&run(&exec, ShellRequest::new("echo done")).await);
    assert_eq!(clean, "done\n");

    let failing = render(&run(&exec, ShellRequest::new("echo out; echo bad 1>&2; exit 2")).await);
    assert_eq!(failing, "out\n[stderr]\nbad\n[exit code: 2]");

    let silent = render(&run(&exec, ShellRequest::new("true")).await);
    assert_eq!(silent, "(no output)");

    let killed = render(&run(&exec, ShellRequest::new("kill -TERM $$")).await);
    assert!(
        killed.ends_with("[killed by signal: SIGTERM]"),
        "a signal is named where an exit code would be: {killed:?}"
    );
}

/// TC-PORT-SHELL-9: a rendered result parses back into its exit status.
///
/// Upstream: `parseExitStatus` ("recovers an exit code", "recovers a signal",
/// "treats absent markers as a clean exit", "leaves marker-like output
/// alone").
///
/// Replay keeps the rendered text and nothing else, so this is the only way a
/// presentation can show the exit of a command that ran in an earlier session.
/// The last requirement is the subtle one: a command whose own output ends
/// with something marker-shaped must not have that line eaten.
///
/// Input: rendered results, and output that merely looks like one.
/// Expected: the code or signal recovered and stripped from the body; a clean
/// exit for text with no marker; and marker-shaped output left in the body
/// when it is not the final line in marker position.
#[test]
fn a_rendered_result_parses_back_into_its_exit_status() {
    let failed = parse_exit("out\n[stderr]\nbad\n[exit code: 2]");
    assert_eq!(failed.code, Some(2));
    assert_eq!(failed.signal, None);
    assert_eq!(failed.body, "out\n[stderr]\nbad");

    let killed = parse_exit("partial\n[killed by signal: SIGKILL]");
    assert_eq!(killed.signal.as_deref(), Some("SIGKILL"));
    assert_eq!(killed.code, None);
    assert_eq!(killed.body, "partial");

    let clean = parse_exit("just output\n");
    assert_eq!(clean.code, Some(0));
    assert_eq!(clean.body, "just output\n");

    let lookalike = parse_exit("the log says [exit code: 9] happened\n");
    assert_eq!(lookalike.body, "the log says [exit code: 9] happened\n");
    assert_eq!(lookalike.code, Some(0), "that was the command's own words");
}

/// TC-PORT-SHELL-10: a command's own argv is never re-split by this seam.
///
/// Upstream passes the command as ONE argv element to `-c` (and to `-Command`)
/// for exactly this reason, and its pwsh package documents it.
///
/// The shell does its own splitting, once. A seam that split first would give
/// every quoted filename two interpretations, and the second one would run
/// under a shell nobody was reading.
///
/// Input: a command whose arguments contain spaces, quotes and a semicolon
/// inside a quoted string.
/// Expected: the shell's own parse is what happens - one file created, named
/// exactly as written - and no second interpretation appears.
#[tokio::test]
async fn a_commands_own_argv_is_never_re_split() {
    let dir = tempfile::tempdir().expect("temp dir");
    let exec = bash_exec(ShellConfig {
        cwd: dir.path().to_path_buf(),
        ..ShellConfig::default()
    });

    let ran = run(
        &exec,
        ShellRequest::new("touch 'a file; with spaces' && ls -1"),
    )
    .await;

    assert!(ran.output.ok(), "{}", render(&ran));
    assert_eq!(ran.output.stdout.text.trim(), "a file; with spaces");
    assert!(
        dir.path().join("a file; with spaces").exists(),
        "the shell parsed it once, and this seam did not parse it again"
    );
}

/// TC-PORT-SHELL-11: a command that starts a daemon does not hold the turn.
///
/// Upstream's disposal terminates managed processes; the hazard restated here
/// is the one this seam meets first, because a background child inherits the
/// output pipe.
///
/// Input: a command that backgrounds a long sleep and exits at once.
/// Expected: the call returns promptly, the command's own exit is reported,
/// and the rendered text tells the model that processes were left running and
/// killed - a fact it cannot otherwise see.
#[tokio::test]
async fn a_command_that_starts_a_daemon_does_not_hold_the_turn() {
    let exec = bash_exec(ShellConfig {
        grace: Duration::from_millis(300),
        ..ShellConfig::default()
    });
    let started = std::time::Instant::now();

    let ran = run(&exec, ShellRequest::new("sleep 30 & echo started")).await;

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "it waited for the daemon"
    );
    assert_eq!(ran.output.code, Some(0));
    assert!(ran.output.swept);
    let text = render(&ran);
    assert!(
        text.contains("killed with its process group"),
        "the model is told what happened to what it started: {text}"
    );
}

/// A bash executor over the given configuration, resolved.
fn bash_exec(config: ShellConfig) -> ShellExec {
    ShellExec::new(Arc::new(Bash::new()), config).expect("this host has a bash")
}

/// Resolve and run one request, which is the only order this seam allows.
async fn run(exec: &ShellExec, request: ShellRequest) -> tetanus_exec::shell::ShellRun {
    let spec = exec.resolve(request).expect("resolved");
    exec.run(&spec).await.expect("the shell started")
}
