//! Test Design Specification: a deployment's hooks, as real children.
//!
//! Feature under test: `tetanus_exec::hooks` - the `HookExecutor` that
//! `crates/hooks` declares and deliberately does not implement. Upstream's
//! equivalent is the `ShellExecutor` its hook runner is handed by whichever
//! bridge is running: its own suite duck-types the seam and leaves the real
//! one to the composition, which is exactly the arrangement here.
//!
//! Approach: real hook scripts on disk, run through `tetanus_hooks::run_hook`
//! rather than through the executor alone. Asserting the executor by itself
//! would prove that a command ran; what has to be true is that the *protocol*
//! reads what came back the way it reads a hook - a `2` blocks, a hook that
//! never ran does not, and neither takes the turn down. That claim spans both
//! crates, so both are in the case.
//!
//! What is not asserted here, and why. Which hook fires on which event, what
//! goes in the payload, and how two hooks' answers merge are the hook lane's
//! (`crates/hooks/tests`, TC-HOOK-*), and their cases run against a recorder
//! because that is the right way to pin a protocol. The recorder is what this
//! file replaces at the composition, not what it re-tests.
//!
//! Environmental needs: a bash on PATH, a writable temp directory. No case
//! reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tetanus_exec::backend::Bash;
use tetanus_exec::hooks::{HookEnv, HookExecConfig, ShellHookExecutor};
use tetanus_exec::shell::ShellConfig;
use tetanus_hooks::runner::{HookExecSpec, HookExecutor, RunHookOptions};
use tetanus_hooks::{run_hook, CommandHook};

/// TC-PORT-HOOK-1: a configured hook is a real child, and what it printed
/// comes back through the protocol.
///
/// The whole point of the seam: until now the only `HookExecutor` in the
/// workspace was a recorder in the hook lane's own suite, so a deployment
/// could configure a hook and nothing would ever run it.
///
/// Input: a hook script that reads its stdin payload and answers with the JSON
/// the protocol understands.
/// Expected: the script really ran, it saw the payload on its stdin, and the
/// decoded outcome carries its decision and its exit code.
#[tokio::test]
async fn a_configured_hook_is_a_real_child_and_its_answer_is_decoded() {
    let dir = tempfile::tempdir().expect("temp dir");
    let hook = script(
        &dir,
        "answer.sh",
        r#"read -r payload
echo "$payload" > "$SEEN"
printf '{"decision":"approve","reason":"because the payload said %s"}' "$(echo "$payload" | tr -cd 'a-zA-Z_')"
"#,
    );
    let seen = dir.path().join("seen.json");
    let executor = executor(HookExecConfig {
        env: HookEnv {
            added: [("SEEN".to_string(), seen.display().to_string())]
                .into_iter()
                .collect(),
            ..HookEnv::default()
        },
        ..config(&dir)
    });

    let ran = run_hook(
        executor.as_ref(),
        &CommandHook {
            command: format!("bash {hook}"),
            timeout_sec: None,
        },
        RunHookOptions {
            payload: json!({ "hook_event_name": "PreToolUse" }),
            ..options()
        },
        &|| 0,
    )
    .await;

    assert_eq!(ran.output.exit_code, Some(0));
    assert!(
        std::fs::read_to_string(&seen)
            .expect("the hook wrote what it read")
            .contains("PreToolUse"),
        "the payload has to reach the hook's stdin"
    );
    assert_eq!(
        ran.output.decision.map(|decision| format!("{decision:?}")),
        Some("Approve".to_string()),
        "the protocol decoded what the hook actually printed"
    );
}

/// TC-PORT-HOOK-2: a hook that blocks really blocks, and a hook that fails
/// some other way does not.
///
/// Exit 2 is the protocol's one blocking channel, and it is the case where an
/// executor's mistake is most expensive in both directions: a wrong `None`
/// lets a call through that a deployment forbade, and a wrong `2` blocks work
/// nobody meant to block. Both are only observable through the decoder, which
/// is why the case drives it.
///
/// Input: one hook exiting 2 with a reason on stderr, one exiting 1.
/// Expected: the first is decoded as blocking with its reason; the second
/// carries its code and blocks nothing.
#[tokio::test]
async fn a_hook_that_exits_two_blocks_and_one_that_exits_one_does_not() {
    let dir = tempfile::tempdir().expect("temp dir");
    let blocking = script(&dir, "deny.sh", "echo 'not on my watch' 1>&2\nexit 2\n");
    let failing = script(&dir, "broken.sh", "echo 'oops' 1>&2\nexit 1\n");
    let executor = executor(config(&dir));

    let denied = fire(&executor, &format!("bash {blocking}")).await;
    assert_eq!(denied.output.exit_code, Some(2));
    assert_eq!(
        denied
            .output
            .decision
            .map(|decision| format!("{decision:?}")),
        Some("Block".to_string()),
        "exit 2 is the blocking channel: {:?}",
        denied.output
    );
    assert!(
        denied
            .output
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("not on my watch")),
        "the reason is what the hook said on stderr: {:?}",
        denied.output.reason
    );

    let broken = fire(&executor, &format!("bash {failing}")).await;
    assert_eq!(broken.output.exit_code, Some(1));
    assert_eq!(
        broken.output.decision, None,
        "a hook that merely failed decides nothing"
    );
}

/// TC-PORT-HOOK-3: a hook that hangs is killed with everything it started, and
/// blocks nothing.
///
/// A hook runs inside a turn, so one that never returns is a harness that
/// never returns. The group kill matters as much as the timeout: a hook that
/// starts a watcher and then hangs would otherwise leave the watcher holding
/// its pipe long after the turn gave up.
///
/// Input: a hook with a one-second budget that starts a long-lived child and
/// then sleeps for five minutes.
/// Expected: the call returns in about a second; the outcome has no exit code,
/// so the protocol reads it as non-blocking; the stderr says it was killed;
/// and the hook's own child is gone.
#[tokio::test]
async fn a_hook_that_hangs_is_killed_with_everything_it_started() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pidfile = dir.path().join("child.pid");
    let hanging = script(
        &dir,
        "hang.sh",
        &format!("sleep 300 & echo $! > {}\nsleep 300\n", pidfile.display()),
    );
    let executor = executor(config(&dir));
    let started = std::time::Instant::now();

    let ran = run_hook(
        executor.as_ref(),
        &CommandHook {
            command: format!("bash {hanging}"),
            timeout_sec: Some(1),
        },
        options(),
        &|| 0,
    )
    .await;

    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the hook held the turn for {:?}",
        started.elapsed()
    );
    assert_eq!(
        ran.output.exit_code, None,
        "a hook nobody waited for has decided nothing"
    );
    assert_eq!(ran.output.decision, None, "so it blocks nothing");
    assert!(
        ran.output.stderr.contains("killed after"),
        "the record should say what happened to it: {:?}",
        ran.output.stderr
    );
    assert!(
        !alive(read_pid(&pidfile)),
        "the hook's own child outlived the hook"
    );
}

/// TC-PORT-HOOK-4: a hook's environment is a list, and this process's secrets
/// are not on it.
///
/// Upstream hands a hook `process.env` minus a denylist, which is the wrong
/// way round: every new secret is exposed until somebody remembers to add it.
/// Nothing is inherited here, so the list says what passes - and a hook that
/// needs more says so in the deployment's own configuration.
///
/// Input: a secret and a `PATH` in this process; a hook printing three
/// variables; one entry added by the configuration and one by the caller.
/// Expected: `PATH` reaches it, the secret does not, and the caller's entry is
/// there.
#[tokio::test]
async fn a_hooks_environment_is_a_list_and_not_a_scrub() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::env::set_var("TETANUS_HOOK_SECRET", "do-not-leak-me");
    let hook = script(
        &dir,
        "env.sh",
        // Every read is defaulted, because `set -u` would otherwise turn "the
        // secret did not reach the hook" into a script that died before
        // printing anything - which is the right answer for the wrong reason.
        "printf 'path=%s secret=%s deployment=%s caller=%s' \
         \"${PATH:+set}\" \"${TETANUS_HOOK_SECRET-}\" \"${FROM_DEPLOYMENT-}\" \"${FROM_CALLER-}\"\n",
    );
    let executor = executor(HookExecConfig {
        env: HookEnv {
            added: [("FROM_DEPLOYMENT".to_string(), "yes".to_string())]
                .into_iter()
                .collect(),
            ..HookEnv::default()
        },
        ..config(&dir)
    });

    let ran = executor
        .run(HookExecSpec {
            command: format!("bash {hook}"),
            timeout_ms: 10_000,
            stdin: String::new(),
            workdir: None,
            env: Some(vec![("FROM_CALLER".to_string(), "also-yes".to_string())]),
        })
        .await
        .expect("the hook ran");

    assert_eq!(
        ran.stdout.trim(),
        "path=set secret= deployment=yes caller=also-yes",
        "the hook sees what was listed and nothing else"
    );
    std::env::remove_var("TETANUS_HOOK_SECRET");
}

/// TC-PORT-HOOK-5: a hook that cannot be run at all is reported as
/// infrastructure, and the turn survives it.
///
/// The seam's contract: `Err` means the harness could not run the thing, as
/// against a hook that ran and said no. The protocol turns the first into a
/// non-blocking outcome with the fault on stderr, because a deployment that
/// mistyped a directory must not have every turn blocked by a hook that never
/// executed.
///
/// Input: a hook whose working directory does not exist.
/// Expected: the executor reports an infrastructure fault; run through the
/// protocol, the outcome has no exit code, blocks nothing, and carries the
/// fault where a person will read it.
#[tokio::test]
async fn a_hook_that_cannot_be_run_is_infrastructure_and_blocks_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let hook = script(&dir, "fine.sh", "echo nothing wrong with me\n");
    let executor = executor(config(&dir));
    let nowhere = dir.path().join("no-such-directory");

    let refused = executor
        .run(HookExecSpec {
            command: format!("bash {hook}"),
            timeout_ms: 10_000,
            stdin: String::new(),
            workdir: Some(nowhere.display().to_string()),
            env: None,
        })
        .await;
    assert!(
        refused.is_err(),
        "an unusable working directory is the harness's problem, not the hook's answer"
    );

    let ran = run_hook(
        executor.as_ref(),
        &CommandHook {
            command: format!("bash {hook}"),
            timeout_sec: None,
        },
        RunHookOptions {
            cwd: Some(nowhere.display().to_string()),
            ..options()
        },
        &|| 0,
    )
    .await;
    assert_eq!(ran.output.exit_code, None);
    assert_eq!(ran.output.decision, None, "a fault decides nothing");
    assert!(
        !ran.output.stderr.is_empty(),
        "somebody has to be able to read why their hook never ran"
    );
}

/// TC-PORT-HOOK-6: a host with no shell refuses while somebody is watching.
///
/// The same rule the rest of this crate follows, applied where it matters
/// most: hooks fire on events deep inside a turn, so a deployment whose shell
/// is missing would otherwise discover it at the least convenient moment,
/// once per event, forever.
///
/// Input: an executor built over a backend whose program is not on this host.
/// Expected: construction fails, naming what is missing.
#[test]
fn a_host_with_no_shell_refuses_at_composition() {
    let refused = ShellHookExecutor::new(
        Arc::new(Bash::at("/nowhere/no-such-bash")),
        HookExecConfig::default(),
    );

    match refused {
        Err(why) => assert!(
            why.to_string().contains("no-such-bash"),
            "the refusal has to name what is missing: {why}"
        ),
        Ok(_) => panic!("a backend that is not there must not build an executor"),
    }
}

// ---------------------------------------------------------------- fixtures

/// An executor over this host's bash.
fn executor(config: HookExecConfig) -> Arc<ShellHookExecutor> {
    ShellHookExecutor::new(Arc::new(Bash::new()), config).expect("this host has a bash")
}

/// Hook configuration rooted in a case's own directory, with budgets short
/// enough that a case waiting for one waits for seconds.
fn config(dir: &tempfile::TempDir) -> HookExecConfig {
    HookExecConfig {
        shell: ShellConfig {
            cwd: dir.path().to_path_buf(),
            timeout: Duration::from_secs(10),
            max_timeout: Duration::from_secs(20),
            grace: Duration::from_millis(200),
            ..ShellConfig::default()
        },
        ..HookExecConfig::default()
    }
}

/// The options a case that cares only about the executor passes.
fn options<'a>() -> RunHookOptions<'a> {
    RunHookOptions {
        payload: json!({ "hook_event_name": "PreToolUse" }),
        env: None,
        cwd: None,
        trailing_newline: true,
        default_timeout_ms: 10_000,
        expected_event: None,
    }
}

/// Fire one hook through the protocol, the way a bridge does.
async fn fire(
    executor: &Arc<ShellHookExecutor>,
    command: &str,
) -> tetanus_hooks::runner::RunHookResult {
    run_hook(
        executor.as_ref(),
        &CommandHook {
            command: command.to_string(),
            timeout_sec: None,
        },
        options(),
        &|| 0,
    )
    .await
}

/// A hook script on disk, executable, and the path to it.
fn script(dir: &tempfile::TempDir, name: &str, body: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, format!("#!/bin/bash\nset -u\n{body}")).expect("wrote the hook");
    path.display().to_string()
}

/// The pid a case's hook recorded, waiting briefly for the file to appear.
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

/// Named so a reader of the fixtures sees what the default list is without
/// opening the crate: this is the whole of what a hook inherits.
const _: fn() -> BTreeMap<String, String> = || HookEnv::empty().added;
