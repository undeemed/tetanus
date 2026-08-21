//! Test Design Specification: the sandbox applied to real commands, ported.
//!
//! Feature under test: `tetanus_sandbox` enforced through `tetanus_exec` - a
//! spawned process behind a kernel boundary, a persistent shell behind one,
//! and what a model is told when the boundary refuses. Upstream pins the same
//! decisions in `packages/sandbox/sandbox-local/tests/local.spec.ts` (its
//! runner wrapping an argv) and in the denial-marker half of
//! `packages/shell/tool-bash/tests/tools.spec.ts`.
//!
//! Approach: **every denial is a real process being denied**. A case spawns a
//! command and reads what the kernel did to it; nothing asserts a policy
//! object. The distinction matters more here than anywhere else in the
//! workspace, because a sandbox that is asserted rather than exercised is
//! indistinguishable from no sandbox at all.
//!
//! What is not restated, and why. The filesystem service is another lane's
//! crate and is not landed here, so "the same policy applied by the file
//! tools" is a named follow-up in
//! `docs/parity-updates/sandbox-policy-and-landlock.md` rather than a guess at
//! an API that does not exist yet. Upstream's escalation flow - a denied
//! command retried once under a wider mode with user approval - needs the
//! approval seam wired to the policy, which is the other named follow-up.
//! Its Windows ACL runner has no backend here (TC-PORT-SANDBOX-11).
//!
//! Environmental needs: Linux with Landlock, a bash on PATH, a writable temp
//! directory. Cases report themselves skipped on a kernel without Landlock
//! rather than passing for the wrong reason. No case reaches a network or an
//! API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tetanus_exec::backend::Bash;
use tetanus_exec::session::SessionConfig;
use tetanus_exec::shell::{render, ShellConfig, ShellError, ShellExec, ShellRequest, ShellRun};
use tetanus_sandbox::{Mode, Policy};

/// TC-PORT-SANDBOX-13: a spawned process cannot write outside the policy.
///
/// Upstream: `local.spec.ts` ("a confined command cannot write outside the
/// workspace").
///
/// The lane's second acceptance criterion, and the one that needed the
/// fork/exec split to be right: the boundary is applied in the child between
/// `fork` and `exec`, so what is confined is the program the model named and
/// not the harness that started it.
///
/// Input: a `workspace-write` policy rooted at a temp workspace, and a command
/// that writes one file inside it and one outside it.
/// Expected: the run reports failure; the inside file exists; the outside file
/// does not; and the harness itself is unaffected - it can still write the
/// path the child could not.
#[tokio::test]
async fn a_spawned_process_cannot_write_outside_the_policy() {
    let Some(workspace) = enforcing() else { return };
    let Some(elsewhere) = outside_every_grant() else {
        return;
    };
    let inside = workspace.path().join("allowed.txt");
    let outside = elsewhere.path().join("forbidden.txt");
    let exec = confined(Mode::WorkspaceWrite, workspace.path());

    let run = run(
        &exec,
        &format!(
            "echo mine > {}; echo theirs > {}",
            inside.display(),
            outside.display()
        ),
    )
    .await;

    assert!(
        !run.output.ok(),
        "the child was allowed to write out: {}",
        render(&run)
    );
    assert_eq!(
        std::fs::read_to_string(&inside).expect("the workspace write landed"),
        "mine\n"
    );
    assert!(
        !outside.exists(),
        "a write outside the policy created {}",
        outside.display()
    );
    // The boundary belongs to the child. A harness that had confined itself
    // would fail here, and every later tool call with it.
    std::fs::write(&outside, b"the harness is not confined").expect("the parent is unrestricted");
    std::fs::remove_file(&outside).expect("tidied");
}

/// TC-PORT-SANDBOX-14: a denied command is told to the model as a denial, not
/// as a mysterious failure.
///
/// Upstream: `sandboxDenialMarker` and the bash tool's rendering of it ("a
/// blocked file operation is reported as a policy denial, not a bug in the
/// command; do not retry another way").
///
/// Without the marker a model reads `Permission denied`, assumes it chose the
/// wrong path, and rewrites a correct command until it gives up. The marker
/// names the mode, which is the fact that tells it what would have to change.
///
/// Input: a `read-only` policy and a command that tries to write.
/// Expected: the rendered result carries the denial marker naming the mode,
/// and still carries the command's own output and exit status.
#[tokio::test]
async fn a_denied_command_is_rendered_as_a_policy_denial() {
    let Some(workspace) = enforcing() else { return };
    let exec = confined(Mode::ReadOnly, workspace.path());

    let run = run(
        &exec,
        &format!(
            "echo attempting; echo blocked > {}",
            workspace.path().join("x").display()
        ),
    )
    .await;
    let text = render(&run);

    assert!(
        text.contains("denied under read-only mode"),
        "the denial has to name the mode: {text}"
    );
    assert!(
        text.contains("policy, not a bug in the command"),
        "and has to say it is policy: {text}"
    );
    assert!(
        text.contains("attempting"),
        "the command's own output survives: {text}"
    );
    assert!(text.contains("[exit code:"), "and its exit status: {text}");
}

/// TC-PORT-SANDBOX-15: a command that stays inside the policy is unaffected,
/// and is not reported as denied.
///
/// The other half of TC-PORT-SANDBOX-14. A marker that appeared on every
/// failure would be worse than none: the model would learn to ignore it, and
/// the one real denial would read like the rest.
///
/// Input: under `workspace-write`, a command that succeeds, and a command that
/// fails for its own reasons.
/// Expected: the first succeeds with no marker; the second reports its own
/// exit code with no denial marker.
#[tokio::test]
async fn work_inside_the_policy_is_not_reported_as_denied() {
    let Some(workspace) = enforcing() else { return };
    let exec = confined(Mode::WorkspaceWrite, workspace.path());

    let fine = run(&exec, "echo working > inside.txt; cat inside.txt").await;
    assert!(fine.output.ok(), "{}", render(&fine));
    assert!(!render(&fine).contains("sandbox:"), "{}", render(&fine));

    let failed = run(&exec, "echo nope 1>&2; exit 9").await;
    let text = render(&failed);
    assert!(text.contains("[exit code: 9]"), "{text}");
    assert!(
        !text.contains("sandbox:"),
        "a command that failed on its own must not read as a denial: {text}"
    );
}

/// TC-PORT-SANDBOX-16: a persistent shell is confined once, and every command
/// it runs inherits it.
///
/// Upstream confines per spawn because each of its bash calls is a spawn; a
/// persistent shell is one process answering many calls, so the boundary is
/// applied to the shell and inherited. Restriction is one-way and inherited by
/// children, which is exactly the property that makes that safe.
///
/// Input: a session under `read-only`, asked to write on its second command.
/// Expected: the first command works, the write is denied, and the session is
/// still usable afterwards - a denial is the kernel refusing an operation, not
/// the shell dying.
#[tokio::test]
async fn a_persistent_shell_is_confined_and_stays_confined() {
    let Some(workspace) = enforcing() else { return };
    let sessions = tetanus_exec::session::ShellSessions::new();
    let session = sessions
        .open(
            Arc::new(Bash::new()),
            SessionConfig {
                cwd: workspace.path().to_path_buf(),
                grace: Duration::from_millis(200),
                sandbox: Policy::new(Mode::ReadOnly, workspace.path()),
                ..SessionConfig::default()
            },
        )
        .await
        .expect("a confined session starts");

    let first = session.run("echo alive").await.expect("ran");
    assert_eq!(first.text, "alive");

    let denied = session
        .run(&format!(
            "echo blocked > {}",
            workspace.path().join("x").display()
        ))
        .await
        .expect("the shell reports what the kernel did");
    assert_ne!(denied.code, 0, "the write was allowed: {}", denied.text);
    assert!(
        denied.text.contains("Permission denied") || denied.text.contains("cannot create"),
        "the shell's own words carry the refusal: {:?}",
        denied.text
    );
    assert!(!workspace.path().join("x").exists());

    let after = session.run("echo still here").await.expect("still usable");
    assert_eq!(
        after.text, "still here",
        "a denial must not kill the session"
    );
}

/// TC-PORT-SANDBOX-17: a host that cannot enforce the policy composes nothing.
///
/// The degraded path at the level a deployment meets it. Upstream fails closed
/// when no backend can serve the requested mode; here the executor is built at
/// composition, so the failure lands there - before a model has been offered a
/// tool that cannot keep its promise.
///
/// Input: a policy demanding what this kernel cannot govern, without accepting
/// partial enforcement.
/// Expected: building the executor fails with the sandbox's own error, naming
/// what is missing; and with partial enforcement accepted, it builds and says
/// so on every result.
#[tokio::test]
async fn a_host_that_cannot_enforce_the_policy_composes_nothing() {
    let Some(workspace) = enforcing() else { return };
    let support = tetanus_sandbox::support().expect("landlock is here");

    if support.governs_network && support.governs_truncate {
        // This kernel can do everything the policy asks, so the refusal cannot
        // be observed; what is asserted instead is that full enforcement is
        // reported as full, and never as partial.
        let exec = confined(Mode::WorkspaceWrite, workspace.path());
        let run = run(&exec, "true").await;
        assert_eq!(
            run.sandbox,
            Some((Mode::WorkspaceWrite, tetanus_sandbox::Enforcement::Full))
        );
        assert!(!render(&run).contains("only part of that policy"));
        return;
    }

    let strict =
        Policy::new(Mode::WorkspaceWrite, workspace.path()).network(tetanus_sandbox::Network::Deny);
    let refused = ShellExec::new(
        Arc::new(Bash::new()),
        ShellConfig {
            cwd: workspace.path().to_path_buf(),
            sandbox: strict.clone(),
            ..ShellConfig::default()
        },
    );
    assert!(
        matches!(refused, Err(ShellError::Sandbox(_))),
        "an under-capable host must not compose a shell that claims to confine"
    );

    let accepted = ShellExec::new(
        Arc::new(Bash::new()),
        ShellConfig {
            cwd: workspace.path().to_path_buf(),
            sandbox: strict.accept_partial_enforcement(),
            ..ShellConfig::default()
        },
    )
    .expect("partial enforcement was accepted in writing");
    let spec = accepted
        .resolve(ShellRequest::new("true"))
        .expect("resolved");
    let run = accepted.run(&spec).await.expect("ran");
    assert!(render(&run).contains("only part of that policy"));
}

/// TC-PORT-SANDBOX-18: `danger-full-access` runs the command with no boundary,
/// and the result says no policy applied.
///
/// The escape hatch, asserted so it cannot rot: a deployment that needs the
/// machine gets the machine, and the result carries no sandbox facts to
/// misread.
///
/// Input: the default configuration, which is `danger-full-access`.
/// Expected: a write outside any workspace succeeds, and the result carries no
/// sandbox facts.
#[tokio::test]
async fn danger_full_access_runs_the_command_unconfined() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(elsewhere) = outside_every_grant() else {
        return;
    };
    let target = elsewhere.path().join("written.txt");
    let exec = ShellExec::new(
        Arc::new(Bash::new()),
        ShellConfig {
            cwd: workspace.path().to_path_buf(),
            ..ShellConfig::default()
        },
    )
    .expect("the default policy composes anywhere");

    let run = run(&exec, &format!("echo out > {}", target.display())).await;

    assert!(run.output.ok(), "{}", render(&run));
    assert_eq!(run.sandbox, None, "an unconfined run reports no policy");
    assert_eq!(std::fs::read_to_string(&target).expect("written"), "out\n");
}

/// TC-PORT-SANDBOX-19: a tool call the policy refuses is a contained tool
/// failure the model reads, and the journal records it.
///
/// Upstream: a denial is an ordinary tool result carrying a marker, never an
/// error that ends the turn.
///
/// This is the lane's integration criterion. Three things have to be true at
/// once: the turn survives, the model is told in words it can act on, and the
/// refusal is on the journal - a denial nobody recorded is a policy nobody can
/// audit.
///
/// Input: a turn whose single `shell` call writes outside a `read-only`
/// policy.
/// Expected: the turn completes; the `tool/result` is `ok: false` carrying the
/// denial marker; the file was not created; and the next request carries the
/// refusal, so the model can choose differently.
#[tokio::test]
async fn a_refused_tool_call_is_contained_and_recorded() {
    let Some(workspace) = enforcing() else { return };
    let target = workspace.path().join("should-not-exist.txt");
    let harness = harness::Harness::new(
        "sandbox-tool",
        workspace.path(),
        Mode::ReadOnly,
        json!({ "command": format!("echo denied > {}", target.display()) }),
    )
    .await;

    harness
        .engine
        .run_turn("write a file")
        .await
        .expect("the turn survived");

    let results = harness.results();
    assert_eq!(results.len(), 1, "one call, one result");
    assert_eq!(results[0]["ok"], json!(false));
    let content = results[0]["content"].as_str().expect("text");
    assert!(
        content.contains("denied under read-only mode"),
        "the journal records why it was refused: {content}"
    );
    assert!(!target.exists(), "the denied write landed anyway");
    assert!(
        harness.second_request_carries("denied under read-only mode"),
        "the model was not told, so it cannot choose differently"
    );
}

// ---------------------------------------------------------------- fixtures

/// A workspace to confine, or `None` after reporting the case skipped on a
/// kernel with no Landlock.
fn enforcing() -> Option<tempfile::TempDir> {
    match tetanus_sandbox::support() {
        Ok(_) => Some(tempfile::tempdir().expect("temp dir")),
        Err(why) => {
            eprintln!("skipped: {why}");
            None
        }
    }
}

/// A shell executor confined to `root` under `mode`.
fn confined(mode: Mode, root: &std::path::Path) -> ShellExec {
    ShellExec::new(
        Arc::new(Bash::new()),
        ShellConfig {
            cwd: root.to_path_buf(),
            timeout: Duration::from_secs(20),
            grace: Duration::from_millis(200),
            sandbox: Policy::new(mode, root),
            ..ShellConfig::default()
        },
    )
    .expect("this host can enforce it")
}

/// Resolve and run one command line.
async fn run(exec: &ShellExec, command: &str) -> ShellRun {
    let spec = exec.resolve(ShellRequest::new(command)).expect("resolved");
    exec.run(&spec).await.expect("the shell started")
}

/// A directory no `workspace-write` policy grants. See the same fixture in
/// `crates/sandbox/tests/upstream_sandbox.rs` for why `tempfile::tempdir()`
/// will not do: it lands under `TMPDIR`, which the mode grants on purpose.
fn outside_every_grant() -> Option<tempfile::TempDir> {
    let granted = |path: &std::path::Path| {
        Policy::new(Mode::WorkspaceWrite, "/nonexistent")
            .writable_roots()
            .iter()
            .any(|root| path.starts_with(root))
    };
    for base in [
        PathBuf::from("/var/tmp"),
        std::env::var_os("HOME").map(PathBuf::from)?.join(".cache"),
    ] {
        if !base.is_dir() {
            continue;
        }
        if let Ok(dir) = tempfile::Builder::new()
            .prefix("tetanus-sandbox-outside-")
            .tempdir_in(&base)
        {
            if !granted(dir.path()) {
                return Some(dir);
            }
        }
    }
    eprintln!("skipped: nowhere outside the granted roots is writable on this host");
    None
}

/// One booted turn driving one scripted `shell` call, for the integration
/// case. Kept here rather than shared with the other suites because it is the
/// only case in this file that needs a turn at all.
mod harness {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::Value;
    use tetanus_core::EventBus;
    use tetanus_exec::backend::Bash;
    use tetanus_exec::session::SessionConfig;
    use tetanus_exec::shell::ShellConfig;
    use tetanus_exec::tools::{ShellTools, SHELL};
    use tetanus_sandbox::{Mode, Policy};
    use tetanus_session::{JsonlSessionLog, SessionLog};
    use tetanus_turn::boot::boot_with;
    use tetanus_turn::interrupt::Interrupt;
    use tetanus_turn::llm::{
        ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse, Role, StreamChunk, Usage,
    };
    use tetanus_turn::tools::ToolCall;
    use tetanus_turn::{TurnConfig, TurnEngine};

    pub struct Harness {
        pub engine: TurnEngine,
        log: Arc<dyn SessionLog>,
        script: Arc<Script>,
        _dir: tempfile::TempDir,
    }

    impl Harness {
        pub async fn new(
            name: &str,
            workspace: &std::path::Path,
            mode: Mode,
            arguments: Value,
        ) -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let bus = EventBus::new();
            let log: Arc<dyn SessionLog> = JsonlSessionLog::create(
                name,
                dir.path().join(format!("{name}.jsonl")),
                bus.clone(),
            )
            .expect("journal");
            let interrupt = Interrupt::new();
            let tools = ShellTools::new(
                Arc::new(Bash::new()),
                ShellConfig {
                    cwd: workspace.to_path_buf(),
                    timeout: Duration::from_secs(20),
                    grace: Duration::from_millis(200),
                    sandbox: Policy::new(mode, workspace),
                    ..ShellConfig::default()
                },
                SessionConfig {
                    cwd: workspace.to_path_buf(),
                    sandbox: Policy::new(mode, workspace),
                    ..SessionConfig::default()
                },
                Arc::clone(&interrupt),
            )
            .expect("this host can enforce it");
            let script = Arc::new(Script::new(arguments));
            let ctx = boot_with(
                bus,
                Arc::clone(&script) as Arc<dyn LlmAdapter>,
                Arc::new(tools.registry()),
                Arc::clone(&log),
                interrupt,
            )
            .expect("boot");
            let engine = TurnEngine::from_context(
                &ctx,
                TurnConfig {
                    model: "scripted-1".into(),
                    ..TurnConfig::default()
                },
            )
            .expect("engine");
            Self {
                engine,
                log,
                script,
                _dir: dir,
            }
        }

        pub fn results(&self) -> Vec<Value> {
            self.log
                .events()
                .iter()
                .filter(|event| event.ty == "tool/result")
                .map(|event| event.data.clone())
                .collect()
        }

        /// Whether the request after the tool call carried `text` back to the
        /// model.
        pub fn second_request_carries(&self, text: &str) -> bool {
            self.script
                .seen
                .lock()
                .expect("no panic holds this lock")
                .get(1)
                .is_some_and(|request| {
                    request
                        .messages
                        .iter()
                        .any(|message| message.role == Role::Tool && message.content.contains(text))
                })
        }
    }

    /// A model that asks for one `shell` call and then answers.
    pub struct Script {
        arguments: Value,
        pub seen: Mutex<Vec<ModelRequest>>,
    }

    impl Script {
        fn new(arguments: Value) -> Self {
            Self {
                arguments,
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmAdapter for Script {
        fn provider(&self) -> &str {
            "scripted"
        }

        fn models(&self) -> Vec<String> {
            vec!["scripted-1".to_string()]
        }

        async fn stream(
            &self,
            request: &ModelRequest,
            sink: &mut dyn ChunkSink,
        ) -> Result<ModelResponse, LlmError> {
            let index = {
                let mut seen = self.seen.lock().expect("no panic holds this lock");
                seen.push(request.clone());
                seen.len() - 1
            };
            let calls = if index == 0 {
                vec![ToolCall {
                    id: "call-1".into(),
                    name: SHELL.into(),
                    arguments: self.arguments.clone(),
                }]
            } else {
                Vec::new()
            };
            let content = if calls.is_empty() { "done" } else { "running" };
            sink.chunk(StreamChunk::Text {
                delta: content.to_string(),
            })
            .await?;
            for call in &calls {
                sink.chunk(StreamChunk::ToolCall { call: call.clone() })
                    .await?;
            }
            Ok(ModelResponse {
                content: content.to_string(),
                reasoning: String::new(),
                tool_calls: calls.clone(),
                finish_reason: if calls.is_empty() {
                    "stop"
                } else {
                    "tool_calls"
                }
                .into(),
                usage: Some(Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                }),
            })
        }
    }
}
