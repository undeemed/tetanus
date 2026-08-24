//! Conformance: running one configured hook.
//!
//! Feature under test: `tetanus_hooks::runner::run_hook` — building the
//! request, and what happens to the turn when the hook cannot be run.
//!
//! Ported from upstream `packages/hooks/hook-protocol/tests/runner.spec.ts`.
//! Case ids TC-HOOK-RUN-1..12. The last two are this port's own.
//!
//! Like upstream's suite this runs against a recording executor, not a real
//! one: `run_hook` is plumbing over the seam, and the real executor belongs to
//! whoever supplies it.

use std::sync::Mutex;

use serde_json::json;
use tetanus_hooks::runner::{
    run_hook, CommandHook, HookExecResult, HookExecSpec, HookExecutor, RunHookOptions,
    DEFAULT_HOOK_TIMEOUT_MS,
};
use tetanus_hooks::types::HookDecision;

/// An executor that records what it was asked and answers however the case says.
struct Recorder {
    specs: Mutex<Vec<HookExecSpec>>,
    answer: Box<dyn Fn() -> Result<HookExecResult, String> + Send + Sync>,
}

impl Recorder {
    fn ok(result: HookExecResult) -> Self {
        Self {
            specs: Mutex::new(Vec::new()),
            answer: Box::new(move || Ok(result.clone())),
        }
    }

    fn failing(fault: &'static str) -> Self {
        Self {
            specs: Mutex::new(Vec::new()),
            answer: Box::new(move || Err(fault.to_owned())),
        }
    }

    fn spec(&self) -> HookExecSpec {
        self.specs.lock().expect("lock")[0].clone()
    }
}

impl HookExecutor for Recorder {
    fn run<'a>(
        &'a self,
        spec: HookExecSpec,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<HookExecResult, String>> + Send + 'a>,
    > {
        self.specs.lock().expect("lock").push(spec);
        let answer = (self.answer)();
        Box::pin(async move { answer })
    }
}

/// A clock that advances five milliseconds per reading, so a run lasts 5ms.
fn clock() -> impl Fn() -> u64 {
    let ticks = Mutex::new(0u64);
    move || {
        let mut t = ticks.lock().expect("lock");
        *t += 5;
        *t
    }
}

/// The options every case starts from.
fn options<'a>() -> RunHookOptions<'a> {
    RunHookOptions {
        payload: json!({}),
        env: None,
        cwd: None,
        trailing_newline: true,
        default_timeout_ms: 1_000,
        expected_event: None,
    }
}

/// A clean run that printed `stdout`.
fn printed(stdout: &str) -> HookExecResult {
    HookExecResult {
        exit_code: Some(0),
        stdout: stdout.to_owned(),
        stderr: String::new(),
    }
}

// --------------------------------------------------------------- the request

/// TC-HOOK-RUN-1: the payload is serialized to stdin, with a trailing newline
/// when the dialect wants one.
#[tokio::test]
async fn the_payload_is_serialized_to_stdin_with_a_trailing_newline() {
    let executor = Recorder::ok(printed(""));
    run_hook(
        &executor,
        &CommandHook {
            command: "my-hook.sh".into(),
            timeout_sec: None,
        },
        RunHookOptions {
            payload: json!({"hook_event_name": "PreToolUse", "tool_name": "Bash"}),
            default_timeout_ms: 60_000,
            ..options()
        },
        &clock(),
    )
    .await;

    let spec = executor.spec();
    assert_eq!(
        spec.stdin,
        "{\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"Bash\"}\n"
    );
    assert_eq!(spec.command, "my-hook.sh");
}

/// TC-HOOK-RUN-2: Codex sends no trailing newline, and that is the whole of
/// that difference.
#[tokio::test]
async fn no_trailing_newline_when_the_dialect_does_not_want_one() {
    let executor = Recorder::ok(printed(""));
    run_hook(
        &executor,
        &CommandHook {
            command: "h".into(),
            timeout_sec: None,
        },
        RunHookOptions {
            payload: json!({"a": 1}),
            trailing_newline: false,
            ..options()
        },
        &clock(),
    )
    .await;
    assert_eq!(executor.spec().stdin, "{\"a\":1}");
}

/// TC-HOOK-RUN-3: environment and working directory reach the request.
#[tokio::test]
async fn env_and_cwd_are_threaded_into_the_request() {
    let executor = Recorder::ok(printed(""));
    run_hook(
        &executor,
        &CommandHook {
            command: "h".into(),
            timeout_sec: None,
        },
        RunHookOptions {
            env: Some(vec![("CLAUDE_PROJECT_DIR".into(), "/proj".into())]),
            cwd: Some("/work".into()),
            ..options()
        },
        &clock(),
    )
    .await;

    let spec = executor.spec();
    assert_eq!(
        spec.env,
        Some(vec![("CLAUDE_PROJECT_DIR".to_owned(), "/proj".to_owned())])
    );
    assert_eq!(spec.workdir.as_deref(), Some("/work"));
}

/// TC-HOOK-RUN-4: a per-hook timeout is in seconds and overrides the default.
///
/// The unit change is the point: the config says seconds, everything inside
/// says milliseconds, and getting it wrong makes a ten-second hook a
/// ten-millisecond one.
#[tokio::test]
async fn a_per_hook_timeout_is_seconds_and_overrides_the_default() {
    let executor = Recorder::ok(printed(""));
    run_hook(
        &executor,
        &CommandHook {
            command: "h".into(),
            timeout_sec: Some(3),
        },
        RunHookOptions {
            default_timeout_ms: 60_000,
            ..options()
        },
        &clock(),
    )
    .await;
    assert_eq!(executor.spec().timeout_ms, 3_000);
}

/// TC-HOOK-RUN-5: with no per-hook timeout the caller's default applies, and
/// the protocol's reference default is ten minutes.
#[tokio::test]
async fn the_default_timeout_applies_when_the_hook_sets_none() {
    let executor = Recorder::ok(printed(""));
    run_hook(
        &executor,
        &CommandHook {
            command: "h".into(),
            timeout_sec: None,
        },
        RunHookOptions {
            default_timeout_ms: 60_000,
            ..options()
        },
        &clock(),
    )
    .await;
    assert_eq!(executor.spec().timeout_ms, 60_000);
    assert_eq!(DEFAULT_HOOK_TIMEOUT_MS, 600_000, "ten minutes");
}

// -------------------------------------------------------------- the outcome

/// TC-HOOK-RUN-6: a clean exit with structured stdout is decoded, and the run
/// is timed.
#[tokio::test]
async fn a_structured_answer_is_decoded_and_the_run_is_timed() {
    let executor = Recorder::ok(printed(
        &json!({"decision": "block", "reason": "no"}).to_string(),
    ));
    let run = run_hook(
        &executor,
        &CommandHook {
            command: "h".into(),
            timeout_sec: None,
        },
        options(),
        &clock(),
    )
    .await;

    assert_eq!(run.output.decision, Some(HookDecision::Block));
    assert_eq!(run.output.reason.as_deref(), Some("no"));
    assert_eq!(run.duration_ms, 5);
}

/// TC-HOOK-RUN-7: a process killed by a signal has no exit code, so it decides
/// nothing.
#[tokio::test]
async fn a_signal_death_has_no_exit_code_and_decides_nothing() {
    let executor = Recorder::ok(HookExecResult {
        exit_code: None,
        stdout: String::new(),
        stderr: "killed".into(),
    });
    let run = run_hook(
        &executor,
        &CommandHook {
            command: "h".into(),
            timeout_sec: None,
        },
        options(),
        &clock(),
    )
    .await;

    assert_eq!(run.output.exit_code, None);
    assert_eq!(run.output.decision, None);
    assert_eq!(run.output.stderr, "killed");
}

/// TC-HOOK-RUN-8: a hook that could not be run at all is a non-blocking error.
/// The turn goes on.
#[tokio::test]
async fn an_infrastructure_fault_becomes_a_non_blocking_outcome() {
    let executor = Recorder::failing("bad workdir: ENOENT");
    let run = run_hook(
        &executor,
        &CommandHook {
            command: "h".into(),
            timeout_sec: None,
        },
        options(),
        &clock(),
    )
    .await;

    assert_eq!(run.output.exit_code, None);
    assert_eq!(run.output.stderr, "bad workdir: ENOENT");
    assert_eq!(run.output.decision, None);
}

/// TC-HOOK-RUN-9: the firing event reaches the codec's guard, so a block for
/// another event is discarded here too.
#[tokio::test]
async fn the_firing_event_reaches_the_codec_guard() {
    let executor = Recorder::ok(printed(
        &json!({
            "hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny"}
        })
        .to_string(),
    ));
    let run = run_hook(
        &executor,
        &CommandHook {
            command: "h".into(),
            timeout_sec: None,
        },
        RunHookOptions {
            expected_event: Some("Stop"),
            ..options()
        },
        &clock(),
    )
    .await;

    assert_eq!(run.output.hook_event_name.as_deref(), Some("PreToolUse"));
    assert_eq!(run.output.decision, None);
}

/// TC-HOOK-RUN-10: a run is timed even when the hook could not be run.
///
/// This port's own. The duration goes on the `hook/result` event, and a fault
/// that reported no duration would leave a gap in the audit trail exactly
/// where someone is looking for one.
#[tokio::test]
async fn a_failed_run_is_still_timed() {
    let executor = Recorder::failing("no shell");
    let run = run_hook(
        &executor,
        &CommandHook {
            command: "h".into(),
            timeout_sec: None,
        },
        options(),
        &clock(),
    )
    .await;
    assert_eq!(run.duration_ms, 5);
}

/// TC-HOOK-RUN-11: a hook cannot block the turn by failing to run.
///
/// This port's own, and the property the whole error path exists for. Neither
/// an infrastructure fault nor a signal death may produce a blocking decision,
/// because only exit 2 blocks and neither of these has an exit code at all. A
/// deployment whose hook binary is missing must not find every tool call
/// denied.
#[tokio::test]
async fn a_hook_that_cannot_run_can_never_block_the_turn() {
    let faults: Vec<Box<dyn HookExecutor>> = vec![
        Box::new(Recorder::failing("spawn failed")),
        Box::new(Recorder::failing("")),
        Box::new(Recorder::ok(HookExecResult {
            exit_code: None,
            stdout: json!({"decision": "block"}).to_string(),
            stderr: "died".into(),
        })),
    ];
    for executor in faults {
        let run = run_hook(
            executor.as_ref(),
            &CommandHook {
                command: "h".into(),
                timeout_sec: None,
            },
            options(),
            &clock(),
        )
        .await;
        assert_eq!(
            run.output.decision, None,
            "a hook that could not run must not decide anything"
        );
    }
}

/// TC-HOOK-RUN-12: an absurd per-hook timeout does not wrap around into a
/// tiny one.
///
/// This port's own. The seconds-to-milliseconds conversion is a multiplication
/// on a value that comes from a configuration file, and the failure that
/// matters is silent: a wrapped timeout would give a hook microseconds to run
/// and read as a hook that always times out.
#[tokio::test]
async fn an_absurd_timeout_saturates_rather_than_wrapping() {
    let executor = Recorder::ok(printed(""));
    run_hook(
        &executor,
        &CommandHook {
            command: "h".into(),
            timeout_sec: Some(u64::MAX),
        },
        options(),
        &clock(),
    )
    .await;
    assert_eq!(executor.spec().timeout_ms, u64::MAX);
}
