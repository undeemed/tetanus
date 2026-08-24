//! Running one configured hook and decoding what it said.
//!
//! This is plumbing over an executor, and deliberately thin: it builds the
//! request, hands it over, and passes the result to [`crate::parse_hook_output`].
//! The interesting rule is what it does when the executor *fails* — see
//! [`run_hook`].
//!
//! # Why the executor is a trait here
//!
//! Running a command with a timeout, a working directory, a scrubbed
//! environment and a cancellation signal is a service this crate consumes, not
//! one it should own — there is a shell crate for that. [`HookExecutor`] is the
//! narrow seam describing exactly what running a hook needs, so this module can
//! be tested against a recorder and the real executor can be supplied by
//! whoever has one. Upstream does the same thing for the same reason: its own
//! suite duck-types `ShellExecutor` and leaves the real one to the bridges.
//!
//! Parity: upstream `packages/hooks/hook-protocol/src/runner.ts`, pinned by its
//! `runner.spec.ts`.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::codec::parse_hook_output;
use crate::types::HookOutput;

/// The per-hook timeout both dialects apply when a hook's config sets none:
/// ten minutes.
///
/// It lives here, once, as the protocol's reference default. Each adapter owns
/// its own configured default and passes it in, so this constant is what that
/// configuration defaults *to*, not what the runner reaches for.
pub const DEFAULT_HOOK_TIMEOUT_MS: u64 = 600_000;

/// One configured command hook.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandHook {
    /// The command line to run.
    pub command: String,
    /// This hook's own timeout, in **seconds** — the unit the config file uses.
    /// The runner converts it; keeping the wire unit here is what stops a
    /// misreading turning ten seconds into ten milliseconds.
    pub timeout_sec: Option<u64>,
}

/// What the runner asks an executor to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookExecSpec {
    /// The command line.
    pub command: String,
    /// How long it may run.
    pub timeout_ms: u64,
    /// The JSON payload written to its stdin.
    pub stdin: String,
    /// Where to run it, when the caller named a directory.
    pub workdir: Option<String>,
    /// Extra environment entries the adapter built.
    pub env: Option<Vec<(String, String)>>,
}

/// What an executor reports back.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookExecResult {
    /// The exit status, or `None` when the process died by signal — there is
    /// no clean code to act on, so it is not one.
    pub exit_code: Option<i32>,
    /// What it printed to stdout.
    pub stdout: String,
    /// What it printed to stderr.
    pub stderr: String,
}

/// The seam the runner needs: run one command, report what happened.
///
/// An implementation returns `Err` only for an *infrastructure* fault — an
/// unusable working directory, a missing shell. A command that ran and failed
/// is a successful execution reporting a non-zero exit.
pub trait HookExecutor: Send + Sync {
    /// Run one command to completion.
    fn run<'a>(
        &'a self,
        spec: HookExecSpec,
    ) -> Pin<Box<dyn Future<Output = Result<HookExecResult, String>> + Send + 'a>>;
}

/// Everything one invocation needs beyond the command itself.
pub struct RunHookOptions<'a> {
    /// The payload written to the hook's stdin, built by the adapter.
    pub payload: Value,
    /// Extra environment entries.
    pub env: Option<Vec<(String, String)>>,
    /// The working directory, when the adapter named one.
    pub cwd: Option<String>,
    /// Whether stdin ends with a newline. Claude Code sends one, Codex does
    /// not, and this is the whole of that difference.
    pub trailing_newline: bool,
    /// The timeout to use when the hook's own config sets none.
    pub default_timeout_ms: u64,
    /// The event that is firing, threaded through to the codec's event guard.
    pub expected_event: Option<&'a str>,
}

/// A decoded outcome, with how long the run took.
#[derive(Debug, Clone, PartialEq)]
pub struct RunHookResult {
    /// What the hook said.
    pub output: HookOutput,
    /// Wall-clock duration, for the `hook/result` event.
    pub duration_ms: u64,
}

/// Run one hook and decode its outcome.
///
/// **This never fails.** A hook that could not be run at all becomes an
/// outcome with no exit code and the fault on stderr, exactly like a hook that
/// ran and died by signal. That is the whole point: hooks are configured by a
/// deployment and a broken one must not take the turn down with it. The
/// codec's rules then make the outcome non-blocking, because only exit 2
/// blocks and neither of these has an exit code at all.
pub async fn run_hook(
    executor: &dyn HookExecutor,
    hook: &CommandHook,
    options: RunHookOptions<'_>,
    // `Send + Sync` because a bridge calls this from inside a bus listener,
    // whose future must be `Send`; a bare `dyn Fn` is neither, which made the
    // runner unusable from the one caller it exists for.
    clock: &(dyn Fn() -> u64 + Send + Sync),
) -> RunHookResult {
    let started = clock();

    let timeout_ms = match hook.timeout_sec {
        // Seconds on the wire, milliseconds everywhere inside.
        Some(seconds) => seconds.saturating_mul(1_000),
        None => options.default_timeout_ms,
    };

    let mut stdin = options.payload.to_string();
    if options.trailing_newline {
        stdin.push('\n');
    }

    let spec = HookExecSpec {
        command: hook.command.clone(),
        timeout_ms,
        stdin,
        workdir: options.cwd,
        env: options.env,
    };

    let output = match executor.run(spec).await {
        Ok(result) => parse_hook_output(
            result.exit_code,
            &result.stdout,
            &result.stderr,
            options.expected_event,
        ),
        // An infrastructure fault is a hook that could not run: no exit code,
        // the fault kept on stderr for the record, and the turn proceeds.
        Err(fault) => parse_hook_output(None, "", &fault, options.expected_event),
    };

    RunHookResult {
        output,
        duration_ms: clock().saturating_sub(started),
    }
}
