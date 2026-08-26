//! Test Design Specification: the model-facing shell tools, through the real
//! turn pipeline.
//!
//! Features under test: `tetanus_exec::tools` - what the model may call, the
//! schemas it reads, what a call answers - and the pipeline those calls run
//! in: grouping, barriers, the parallel cap, results committed in model order,
//! the result reaching the next request, and an interrupt that terminates a
//! running child. Upstream pins the tool half in
//! `packages/shell/tool-bash/tests/tools.spec.ts`,
//! `packages/shell/tool-bash/tests/integration.spec.ts`,
//! `packages/shell/tool-bash-persistent/tests/tools.spec.ts` and
//! `packages/terminal/tool-terminal/tests/tools.spec.ts`.
//!
//! Approach: a real `TurnEngine`, a real journal, real shells, and a scripted
//! adapter standing in for the model. Asserting a tool against its own
//! `execute` would leave the interesting half untested: a tool is only useful
//! if the loop dispatches it, records it, and carries its result into the next
//! request, and every one of those is the engine's doing rather than the
//! tool's.
//!
//! What is not restated, and why. Upstream's `run_in_background` half needs
//! the job store this phase has not built (`docs/parity.md`, `jobs/*`), and
//! its sandbox-escalation arguments (`sandbox_permissions`, `justification`)
//! need the sandbox mode vocabulary that stays phase ③. Its presentation
//! callbacks (`presentCall`, `presentResult`, terminal cards) belong to the
//! presentation lane and are not part of the engine boundary. Its
//! owner-scoping - a session belongs to one agent, and another agent is told
//! it does not exist - has no counterpart until a session has an owner.
//!
//! Environmental needs: a bash on PATH, a writable temp directory. No case
//! reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tetanus_core::EventBus;
use tetanus_exec::backend::Bash;
use tetanus_exec::session::SessionConfig;
use tetanus_exec::shell::ShellConfig;
use tetanus_exec::tools::{
    ShellTools, JOB_KILL, JOB_LIST, JOB_OUTPUT, SHELL, SHELL_CLOSE, SHELL_LIST, SHELL_OPEN,
    SHELL_RUN,
};
use tetanus_session::{JsonlSessionLog, SessionLog};
use tetanus_turn::boot::boot_with;
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::llm::{
    ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse, Role, StreamChunk, Usage,
};
use tetanus_turn::tools::ToolCall;
use tetanus_turn::{TurnConfig, TurnEngine};

/// TC-PORT-SHELL-12: the eight tools are registered, and each advertises a
/// schema a model can call.
///
/// Upstream: "registers the bash tool with its parameter schema", and the
/// terminal family's own registrations.
///
/// A tool with no schema is a tool the model cannot call correctly, and a
/// schema that does not name its required arguments is a tool it will call
/// wrongly on its first attempt.
///
/// Input: a registry the shell tools registered on.
/// Expected: the eight names - the five shell tools and the three job tools,
/// which are declared whether or not a store is composed so the catalogue and
/// a run cannot offer different sets; `shell` requires `command` and
/// advertises `workdir` and `timeout_ms`; `shell_run` requires both its
/// arguments; `shell_list` takes none.
#[test]
fn the_eight_tools_are_registered_with_callable_schemas() {
    let tools = shell_tools(&std::env::temp_dir(), Interrupt::new());
    let registry = tools.registry();

    assert_eq!(
        registry.names().cloned().collect::<Vec<_>>(),
        vec![
            JOB_KILL,
            JOB_LIST,
            JOB_OUTPUT,
            SHELL,
            SHELL_CLOSE,
            SHELL_LIST,
            SHELL_OPEN,
            SHELL_RUN
        ],
        "the registry offers them in one settled order"
    );

    let schemas = registry.schemas();
    let shell = schema(&schemas, SHELL);
    assert_eq!(shell.parameters["required"], json!(["command"]));
    for advertised in ["command", "description", "workdir", "timeout_ms"] {
        assert!(
            shell.parameters["properties"].get(advertised).is_some(),
            "`{advertised}` is not advertised, so no model will send it"
        );
    }
    assert!(
        shell.description.contains("[exit code: N]"),
        "the description has to teach the marker the model must read: {}",
        shell.description
    );

    let run = schema(&schemas, SHELL_RUN);
    assert_eq!(run.parameters["required"], json!(["session_id", "command"]));
    let list = schema(&schemas, SHELL_LIST);
    assert_eq!(list.parameters["properties"], json!({}));
}

/// TC-PORT-SHELL-13: a real command runs end to end through a turn, and its
/// result reaches the next request.
///
/// Upstream: `tool-bash/tests/integration.spec.ts` ("the agent runs a command
/// and reads its output"), over the loop contract its own
/// `agent-loop/tests/loop.spec.ts` pins.
///
/// This is the acceptance criterion of the whole lane in one case: a model
/// asks for a shell command, the command really runs, and the model's next
/// request carries what it printed. A tool that works in isolation and never
/// reaches the next request is a tool the model cannot use.
///
/// Input: a scripted model that calls `shell` with a command writing a file
/// and printing a line, then answers.
/// Expected: the file exists; the journal records the call and its result; the
/// second request carries a tool message holding the command's output; and the
/// turn's answer quotes it.
#[tokio::test]
async fn a_real_command_runs_through_a_turn_and_reaches_the_next_request() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = Script::new(vec![Step::Call(ToolCall {
        id: "call-1".into(),
        name: SHELL.into(),
        arguments: json!({ "command": "echo ran-for-real > witness.txt; echo printed-this" }),
    })]);
    let harness = Harness::new("shell-e2e", dir.path(), script.clone()).await;

    let outcome = harness
        .engine
        .run_turn("run something")
        .await
        .expect("turn");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("witness.txt")).expect("the command wrote it"),
        "ran-for-real\n"
    );
    let results = harness.results();
    assert_eq!(results.len(), 1, "one call, one result");
    assert_eq!(results[0]["name"], json!(SHELL));
    assert_eq!(results[0]["ok"], json!(true));
    assert_eq!(results[0]["content"], json!("printed-this\n"));

    let second = script.request(1).expect("a second request was made");
    let carried = second
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("the tool result is in the next request");
    assert_eq!(carried.content, "printed-this\n");
    assert!(outcome.content.contains("printed-this"));
}

/// TC-PORT-SHELL-14: a command that fails is a result the model reads, not a
/// broken turn.
///
/// Upstream: "a non-zero exit is reported, not thrown".
///
/// Input: a scripted model calling `shell` with a command that prints to both
/// streams and exits 4.
/// Expected: the turn completes; the result carries stdout, the marked stderr
/// section and `[exit code: 4]`; and the next request carries the same text.
#[tokio::test]
async fn a_command_that_fails_is_a_result_the_model_reads() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = Script::new(vec![Step::Call(ToolCall {
        id: "call-1".into(),
        name: SHELL.into(),
        arguments: json!({ "command": "echo out; echo bad 1>&2; exit 4" }),
    })]);
    let harness = Harness::new("shell-failure", dir.path(), script.clone()).await;

    harness
        .engine
        .run_turn("try it")
        .await
        .expect("the turn survived");

    let results = harness.results();
    let content = results[0]["content"].as_str().expect("text");
    assert_eq!(content, "out\n[stderr]\nbad\n[exit code: 4]");
    assert_eq!(
        results[0]["ok"],
        json!(false),
        "the flag says the command failed; the text says how"
    );
    let carried = script.request(1).expect("a second request");
    assert!(carried
        .messages
        .iter()
        .any(|message| message.role == Role::Tool && message.content.contains("[exit code: 4]")));
}

/// TC-PORT-SHELL-15: a command that hangs is killed by its timeout, its
/// process group is gone, and the turn recovers.
///
/// Upstream: "kills the command when the timeout expires" plus the loop's own
/// "a failing tool does not fail the turn".
///
/// Three claims in one case because they are one behaviour: the budget has to
/// end the command, the kill has to reach what the command started, and the
/// turn has to keep going with something the model can act on. Any two without
/// the third is a harness that hangs, leaks, or lies.
///
/// Input: a scripted model calling `shell` with a short `timeout_ms` on a
/// command that records a grandchild's pid and sleeps.
/// Expected: the turn completes normally; the result is a failure carrying the
/// timeout marker and the output printed first; the grandchild is gone; and
/// the next request carries the failure.
#[tokio::test]
async fn a_hanging_command_is_killed_and_the_turn_recovers() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pidfile = dir.path().join("grandchild.pid");
    let script = Script::new(vec![Step::Call(ToolCall {
        id: "call-1".into(),
        name: SHELL.into(),
        arguments: json!({
            "command": format!("echo starting; sleep 30 & echo $! > {}; sleep 30", pidfile.display()),
            "timeout_ms": 400,
        }),
    })]);
    let harness = Harness::new("shell-timeout", dir.path(), script.clone()).await;

    let started = std::time::Instant::now();
    let outcome = harness
        .engine
        .run_turn("hang")
        .await
        .expect("the turn recovered");

    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the turn waited for the sleep: {:?}",
        started.elapsed()
    );
    let results = harness.results();
    let content = results[0]["content"].as_str().expect("text");
    assert!(
        content.contains("starting"),
        "the early output survived: {content}"
    );
    assert!(
        content.contains("[timed out after 400ms]"),
        "the model is told what happened: {content}"
    );
    assert_eq!(results[0]["ok"], json!(false));
    assert!(
        !alive(read_pid(&pidfile)),
        "the process group outlived the timeout"
    );
    assert!(
        script.request(1).is_some() && !outcome.content.is_empty(),
        "the turn went on to a second step"
    );
}

/// TC-PORT-TERM-13: a persistent session keeps state across two tool calls in
/// one turn.
///
/// Upstream: `tool-bash-persistent/tests/tools.spec.ts` ("state persists
/// across two tool calls").
///
/// The tool-level restatement of TC-PORT-TERM-1: the model, not a Rust caller,
/// opens the session and names it again in the next call. The id has to
/// survive in the model's own hands for that to work.
///
/// Input: a scripted model that opens a session, `cd`s and exports in one
/// call, then reads both back in another, then closes it.
/// Expected: the second command sees the first's directory and variable; the
/// close reports the session gone; and `shell_list` afterwards lists none.
#[tokio::test]
async fn a_session_keeps_state_across_two_tool_calls_in_one_turn() {
    let dir = tempfile::tempdir().expect("temp dir");
    let inner = dir.path().join("inner");
    std::fs::create_dir(&inner).expect("made");
    let script = Script::new(vec![
        Step::Call(ToolCall {
            id: "open".into(),
            name: SHELL_OPEN.into(),
            arguments: json!({}),
        }),
        Step::CallFrom(Box::new(|earlier| ToolCall {
            id: "first".into(),
            name: SHELL_RUN.into(),
            arguments: json!({
                "session_id": session_id(earlier),
                "command": "cd inner; export CARRIED=yes; echo set",
            }),
        })),
        Step::CallFrom(Box::new(|earlier| ToolCall {
            id: "second".into(),
            name: SHELL_RUN.into(),
            arguments: json!({
                "session_id": session_id(earlier),
                "command": "basename \"$PWD\"; echo \"$CARRIED\"",
            }),
        })),
        Step::CallFrom(Box::new(|earlier| ToolCall {
            id: "close".into(),
            name: SHELL_CLOSE.into(),
            arguments: json!({ "session_id": session_id(earlier) }),
        })),
        Step::Call(ToolCall {
            id: "list".into(),
            name: SHELL_LIST.into(),
            arguments: json!({}),
        }),
    ]);
    let harness = Harness::new("shell-session", dir.path(), script.clone()).await;

    harness
        .engine
        .run_turn("use a persistent shell")
        .await
        .expect("turn");

    let results = harness.results();
    assert_eq!(results.len(), 5, "five calls, five results");
    let second = results[2]["content"].as_str().expect("text");
    assert_eq!(
        second.lines().collect::<Vec<_>>(),
        vec!["inner", "yes"],
        "the second call ran in the shell the first one left behind"
    );
    assert!(results[3]["content"]
        .as_str()
        .expect("text")
        .starts_with("closed session"));
    assert_eq!(
        results[4]["content"],
        json!("no shell sessions are open"),
        "the closed session is not listed"
    );
}

/// TC-PORT-TERM-14: a killed shell is reported to the model, and nothing is
/// restarted behind it.
///
/// Upstream resets the shell and tells the model it was reset; this reports it
/// and does not reset. The case pins the difference where the model can see
/// it, because this is the behaviour a model has to reason about.
///
/// Input: a scripted model that opens a session, kills the shell from inside
/// it, then tries another command in the same session, then lists.
/// Expected: the killing call answers with a failure naming what happened; the
/// following call is refused with the same reason rather than quietly
/// succeeding in a fresh shell; and the listing shows the session as gone.
#[tokio::test]
async fn a_killed_shell_is_reported_and_nothing_is_restarted() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = Script::new(vec![
        Step::Call(ToolCall {
            id: "open".into(),
            name: SHELL_OPEN.into(),
            arguments: json!({}),
        }),
        Step::CallFrom(Box::new(|earlier| ToolCall {
            id: "kill".into(),
            name: SHELL_RUN.into(),
            arguments: json!({
                "session_id": session_id(earlier),
                "command": "echo goodbye; kill -KILL $$",
            }),
        })),
        Step::CallFrom(Box::new(|earlier| ToolCall {
            id: "after".into(),
            name: SHELL_RUN.into(),
            arguments: json!({
                "session_id": session_id(earlier),
                "command": "echo still here",
            }),
        })),
        Step::Call(ToolCall {
            id: "list".into(),
            name: SHELL_LIST.into(),
            arguments: json!({}),
        }),
    ]);
    let harness = Harness::new("shell-death", dir.path(), script.clone()).await;

    harness
        .engine
        .run_turn("kill the shell")
        .await
        .expect("turn");

    let results = harness.results();
    let died = results[1]["content"].as_str().expect("text");
    assert_eq!(results[1]["ok"], json!(false));
    assert!(
        died.contains("goodbye") && died.contains("not restarted"),
        "the model is told what it printed and that nothing was restarted: {died}"
    );
    let after = results[2]["content"].as_str().expect("text");
    assert_eq!(results[2]["ok"], json!(false), "a dead session stays dead");
    assert!(
        !after.contains("still here"),
        "the command must not have run in a shell nobody asked for: {after}"
    );
    assert!(
        results[3]["content"]
            .as_str()
            .expect("text")
            .contains("gone:"),
        "the listing says the session is gone: {}",
        results[3]["content"]
    );
}

/// TC-PORT-SHELL-16: the pipeline runs these tools as their modes say, and
/// commits every result in model order.
///
/// Upstream: `core/agent-loop/tests/tool-calls.spec.ts` ("exclusive calls are
/// barriers", "results are committed in the order the model asked for them"),
/// restated over tools that really touch the world.
///
/// `shell` is a barrier because a command can write anything; `shell_list`
/// reads a registry and is safe beside its siblings. The ordering claim is the
/// one that matters to a model: it asked in an order, and the journal has to
/// answer in that order however the work settled.
///
/// Input: one step asking for five calls that interleave the two modes, with
/// the barriers deliberately slow in reverse order.
/// Expected: five results, in the order asked, each holding its own output;
/// and the two commands that write to one file did not overlap - the file
/// records their strict alternation.
#[tokio::test]
async fn the_pipeline_runs_these_tools_as_their_modes_say() {
    let dir = tempfile::tempdir().expect("temp dir");
    let log = dir.path().join("order.log");
    let slow = |name: &str, seconds: &str| {
        json!({
            "command": format!(
                "echo start-{name} >> {log}; sleep {seconds}; echo end-{name} >> {log}; echo {name}",
                log = log.display()
            ),
        })
    };
    let script = Script::new(vec![Step::Calls(vec![
        ToolCall {
            id: "a".into(),
            name: SHELL.into(),
            arguments: slow("a", "0.4"),
        },
        ToolCall {
            id: "b".into(),
            name: SHELL_LIST.into(),
            arguments: json!({}),
        },
        ToolCall {
            id: "c".into(),
            name: SHELL.into(),
            arguments: slow("c", "0.1"),
        },
        ToolCall {
            id: "d".into(),
            name: SHELL_LIST.into(),
            arguments: json!({}),
        },
        ToolCall {
            id: "e".into(),
            name: SHELL.into(),
            arguments: slow("e", "0.05"),
        },
    ])]);
    let harness = Harness::new("shell-pipeline", dir.path(), script).await;

    harness
        .engine
        .run_turn("do several things")
        .await
        .expect("turn");

    let results = harness.results();
    let answered: Vec<&str> = results
        .iter()
        .map(|result| result["call_id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        answered,
        vec!["a", "b", "c", "d", "e"],
        "results commit in the order the model asked, whatever order they finished in"
    );
    assert_eq!(results[0]["content"], json!("a\n"));
    assert_eq!(results[2]["content"], json!("c\n"));
    assert_eq!(results[4]["content"], json!("e\n"));

    let interleaving = std::fs::read_to_string(&log).expect("the commands wrote it");
    assert_eq!(
        interleaving.lines().collect::<Vec<_>>(),
        vec!["start-a", "end-a", "start-c", "end-c", "start-e", "end-e"],
        "a barrier ran alone: no command started before the one before it finished"
    );
}

/// TC-PORT-SHELL-17: an interrupt terminates the running child.
///
/// Upstream: "the tool-call abort signal kills the command"; here the signal
/// is the turn's own interrupt, shared with the tools by the composition.
///
/// An interrupt that only lands at the step boundary leaves the command
/// running: the turn ends, nobody reads the answer, and a `sleep 300` and
/// everything it started keep the workspace busy. Sharing the switch is what
/// makes stopping a turn stop its work.
///
/// Input: a turn whose single call is a long command recording a grandchild's
/// pid, interrupted shortly after it starts.
/// Expected: the turn returns promptly; the result says it was interrupted;
/// and the child and its grandchild are gone.
#[tokio::test]
async fn an_interrupt_terminates_the_running_child() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pidfile = dir.path().join("child.pid");
    let script = Script::new(vec![Step::Call(ToolCall {
        id: "call-1".into(),
        name: SHELL.into(),
        arguments: json!({
            "command": format!("sleep 60 & echo $! > {}; sleep 60", pidfile.display()),
        }),
    })]);
    let harness = Harness::new("shell-interrupt", dir.path(), script).await;

    let watching = pidfile.clone();
    let interrupt = Arc::clone(&harness.interrupt);
    tokio::spawn(async move {
        // Interrupt once the command is really running, so the case is about
        // stopping work rather than about never starting it.
        for _ in 0..300 {
            if watching.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        interrupt.stop();
    });

    let started = std::time::Instant::now();
    harness
        .engine
        .run_turn("start something long")
        .await
        .expect("turn");

    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the interrupt did not reach the command: {:?}",
        started.elapsed()
    );
    let results = harness.results();
    let content = results[0]["content"].as_str().expect("text");
    assert!(
        content.contains("[interrupted]"),
        "the result says why it stopped: {content}"
    );
    assert!(
        !alive(read_pid(&pidfile)),
        "the child outlived the interrupt"
    );
}

/// TC-PORT-SHELL-18: arguments the model got wrong are refused with a message
/// it can act on.
///
/// Upstream: "rejects an empty command", "rejects a non-positive timeout".
///
/// A tool that silently defaulted a bad argument would run something the model
/// did not ask for; a tool that failed without saying which argument was wrong
/// makes the model guess. Both are worse than a refusal that names the field.
///
/// Input: a call with no command, one with an empty command, one with a
/// zero timeout, one naming a session that was never opened.
/// Expected: each is a failed result naming what was wrong; the turn survives
/// all four.
#[tokio::test]
async fn arguments_the_model_got_wrong_are_refused_with_a_reason() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = Script::new(vec![Step::Calls(vec![
        ToolCall {
            id: "no-command".into(),
            name: SHELL.into(),
            arguments: json!({}),
        },
        ToolCall {
            id: "empty".into(),
            name: SHELL.into(),
            arguments: json!({ "command": "   " }),
        },
        ToolCall {
            id: "zero-timeout".into(),
            name: SHELL.into(),
            arguments: json!({ "command": "true", "timeout_ms": 0 }),
        },
        ToolCall {
            id: "no-session".into(),
            name: SHELL_RUN.into(),
            arguments: json!({ "session_id": "shell-404", "command": "true" }),
        },
    ])]);
    let harness = Harness::new("shell-arguments", dir.path(), script).await;

    harness
        .engine
        .run_turn("get it wrong")
        .await
        .expect("the turn survived");

    let results = harness.results();
    assert_eq!(results.len(), 4);
    for result in &results {
        assert_eq!(result["ok"], json!(false), "{result}");
    }
    assert!(results[0]["content"]
        .as_str()
        .expect("text")
        .contains("missing `command`"));
    assert!(results[1]["content"]
        .as_str()
        .expect("text")
        .contains("`command` must not be empty"));
    assert!(results[2]["content"]
        .as_str()
        .expect("text")
        .contains("`timeout_ms` must be a positive"));
    assert!(
        results[3]["content"]
            .as_str()
            .expect("text")
            .contains("shell-404"),
        "the refusal names the session that is not there: {}",
        results[3]["content"]
    );
}

/// TC-PORT-SHELL-19: a host with no shell still advertises `shell`, and every
/// call answers the deployment fault.
///
/// Not an upstream case: upstream's composition fails to load when its
/// executor cannot be mounted, because a host without a shell is a
/// misconfiguration there. A tetanus binary has other tools that still work,
/// so it starts - and this is the case that keeps the shell's absence loud
/// rather than invisible.
///
/// A tool that quietly vanished would leave the model unable to tell "this
/// build has no shell" from "this deployment is broken", and it would report
/// neither to the person who could fix it.
///
/// Input: the tools registered against a bash pinned to a program that is not
/// there, and a model that calls `shell` anyway.
/// Expected: `shell` is still registered, its description names the fault, and
/// the call answers a failure naming the missing program.
#[tokio::test]
async fn a_host_with_no_shell_still_advertises_the_tool_and_explains() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut registry = tetanus_turn::tools::ToolRegistry::new();
    ShellTools::register_or_explain(
        &mut registry,
        Arc::new(Bash::at("/nowhere/bin/bash")),
        ShellConfig {
            cwd: dir.path().to_path_buf(),
            ..ShellConfig::default()
        },
        SessionConfig::default(),
        Interrupt::new(),
        // No job store: this suite is about the five shell tools.
        None,
    );

    assert_eq!(
        registry.names().cloned().collect::<Vec<_>>(),
        vec![SHELL],
        "the one tool it can honestly offer is the one that explains itself"
    );
    let advertised = registry.schemas()[0].description.clone();
    assert!(
        advertised.contains("/nowhere/bin/bash"),
        "the description names what is missing: {advertised}"
    );

    let refused = registry
        .execute(&ToolCall {
            id: "call-1".into(),
            name: SHELL.into(),
            arguments: json!({ "command": "true" }),
        })
        .await
        .expect_err("there is no shell to run it");
    assert!(
        refused
            .to_string()
            .contains("no other shell was substituted"),
        "the refusal rules out the silent fallback: {refused}"
    );
}

// ---------------------------------------------------------------- fixtures

/// A call written from what earlier results said - the session id
/// `shell_open` minted, which a real model reads out of the conversation the
/// same way.
type CallFromEarlier = Box<dyn Fn(&[String]) -> ToolCall + Send + Sync>;

/// One step of the scripted model: the calls it asks for, or nothing.
enum Step {
    Call(ToolCall),
    Calls(Vec<ToolCall>),
    CallFrom(CallFromEarlier),
}

/// A model that asks for exactly what a case wrote, one step at a time, and
/// records every request it was given.
#[derive(Clone)]
struct Script {
    steps: Arc<Vec<Step>>,
    seen: Arc<Mutex<Vec<ModelRequest>>>,
}

impl Script {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Arc::new(steps),
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The request the engine dispatched for step `index`, if it got that far.
    fn request(&self, index: usize) -> Option<ModelRequest> {
        self.seen
            .lock()
            .expect("no panic holds this lock")
            .get(index)
            .cloned()
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
        // Whatever the earlier tool results said, in the order they were
        // answered: how a scripted step names a session the model opened.
        let earlier: Vec<String> = request
            .messages
            .iter()
            .filter(|message| message.role == Role::Tool)
            .map(|message| message.content.clone())
            .collect();

        let calls: Vec<ToolCall> = match self.steps.get(index) {
            Some(Step::Call(call)) => vec![call.clone()],
            Some(Step::Calls(calls)) => calls.clone(),
            Some(Step::CallFrom(make)) => vec![make(&earlier)],
            None => Vec::new(),
        };

        let content = if calls.is_empty() {
            format!("done: {}", earlier.last().cloned().unwrap_or_default())
        } else {
            "running that now".to_string()
        };
        sink.chunk(StreamChunk::Text {
            delta: content.clone(),
        })
        .await?;
        for call in &calls {
            sink.chunk(StreamChunk::ToolCall { call: call.clone() })
                .await?;
        }
        Ok(ModelResponse {
            content,
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

/// One booted engine with the shell tools on it, writing to a temp journal.
struct Harness {
    engine: TurnEngine,
    log: Arc<dyn SessionLog>,
    interrupt: Arc<Interrupt>,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new(name: &str, workspace: &std::path::Path, script: Script) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let bus = EventBus::new();
        let log: Arc<dyn SessionLog> =
            JsonlSessionLog::create(name, dir.path().join(format!("{name}.jsonl")), bus.clone())
                .expect("journal");
        // One switch for the loop and for the tools: an interrupt that does not
        // reach a running command is an interrupt that leaves it running.
        let interrupt = Interrupt::new();
        let tools = shell_tools(workspace, Arc::clone(&interrupt));
        let ctx = boot_with(
            bus,
            Arc::new(script),
            Arc::new(tools.registry()),
            Arc::clone(&log),
            Arc::clone(&interrupt),
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
            interrupt,
            _dir: dir,
        }
    }

    /// Every `tool/result` on the journal, in the order it was committed.
    fn results(&self) -> Vec<Value> {
        self.log
            .events()
            .iter()
            .filter(|event| event.ty == "tool/result")
            .map(|event| event.data.clone())
            .collect()
    }
}

/// The shell tools over a bash backend rooted at `workspace`, with budgets
/// short enough that a case waiting for one waits for milliseconds.
fn shell_tools(workspace: &std::path::Path, interrupt: Arc<Interrupt>) -> Arc<ShellTools> {
    ShellTools::new(
        Arc::new(Bash::new()),
        ShellConfig {
            cwd: workspace.to_path_buf(),
            timeout: Duration::from_secs(20),
            max_timeout: Duration::from_secs(30),
            grace: Duration::from_millis(200),
            ..ShellConfig::default()
        },
        SessionConfig {
            cwd: workspace.to_path_buf(),
            timeout: Duration::from_secs(20),
            grace: Duration::from_millis(200),
            ..SessionConfig::default()
        },
        interrupt,
    )
    .expect("this host has a bash")
}

/// The session id an earlier `shell_open` result announced.
fn session_id(earlier: &[String]) -> String {
    let opened = earlier
        .iter()
        .find(|text| text.starts_with("opened "))
        .expect("a session was opened first");
    opened
        .split_whitespace()
        .nth(3)
        .expect("the id is the fourth word")
        .to_string()
}

/// One named schema out of a registry's list.
fn schema<'a>(
    schemas: &'a [tetanus_turn::tools::ToolSchema],
    name: &str,
) -> &'a tetanus_turn::tools::ToolSchema {
    schemas
        .iter()
        .find(|schema| schema.name == name)
        .unwrap_or_else(|| panic!("{name} is not registered"))
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
