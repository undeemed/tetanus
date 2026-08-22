//! Test Design Specification: the six terminal tools, through the real turn
//! pipeline.
//!
//! Features under test: `tetanus_exec::terminal_tools` - what the model may
//! call, the schemas it reads, and what each call answers - and the pipeline
//! those calls run in: barriers, parallel-safe calls, results committed in
//! model order, and the result reaching the next request. Upstream pins the
//! tool half in `packages/terminal/tool-terminal/tests/tools.spec.ts` and its
//! rendering in `tests/render.spec.ts`.
//!
//! Approach: a real `TurnEngine`, a real journal, real terminals, and a
//! scripted adapter standing in for the model - the same harness
//! `upstream_tools.rs` uses for the `shell_*` family, for the same reason: a
//! tool that works in isolation and never reaches the next request is a tool
//! the model cannot use, and everything between the two is the engine's doing.
//!
//! What is not restated, and why. Upstream's `run_in_background` mode needs
//! the job store this phase has not built, so no background argument is
//! advertised rather than one that cannot be collected. Its presentation
//! callbacks (`presentCall`, `presentResult`, the terminal card) belong to the
//! presentation lane and are not part of the engine boundary. Its
//! `finalizeContent` cap is served by the renderer's own bound here, because
//! tetanus tools return text rather than a content-block list a later hook
//! rewrites.
//!
//! Environmental needs: Linux with `/dev/ptmx`, a bash on PATH, a writable
//! temp directory. No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tetanus_core::EventBus;
use tetanus_exec::backend::Bash;
use tetanus_exec::terminal::TerminalConfig;
use tetanus_exec::terminal_tools::{
    TerminalTools, TERMINAL_CLOSE, TERMINAL_LIST, TERMINAL_OPEN, TERMINAL_READ, TERMINAL_SEND,
    TERMINAL_SIGNAL,
};
use tetanus_exec::terminals::{Owner, Terminals};
use tetanus_session::{JsonlSessionLog, SessionLog};
use tetanus_turn::boot::boot_with;
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::llm::{
    ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse, Role, StreamChunk, Usage,
};
use tetanus_turn::tools::{ToolCall, ToolMode};
use tetanus_turn::{TurnConfig, TurnEngine};

/// TC-PORT-TERM-34: the six tools are registered, and each advertises a schema
/// a model can call.
///
/// Upstream: `tool-terminal/tests/tools.spec.ts` ("registers every terminal
/// tool with its parameter schema").
///
/// A schema that does not name its required arguments is a tool the model will
/// call wrongly on its first attempt, and one that advertises an argument the
/// deployment cannot honour costs it a call to find out. The `signal` list is
/// closed for the second reason: a model that can name any signal will
/// eventually name one that leaves a shell stopped for ever.
///
/// Input: a registry the terminal tools registered on.
/// Expected: the six names; `terminal_send` requires its session and text and
/// advertises `submit`; `terminal_signal` advertises exactly the five allowed
/// signals; `terminal_list` takes no arguments; and no tool advertises a
/// background mode this phase cannot collect.
#[test]
fn the_six_tools_are_registered_with_callable_schemas() {
    let (_dir, tools) = terminal_tools(Interrupt::new());
    let registry = tools.registry();

    assert_eq!(
        registry.names().cloned().collect::<Vec<_>>(),
        vec![
            TERMINAL_CLOSE,
            TERMINAL_LIST,
            TERMINAL_OPEN,
            TERMINAL_READ,
            TERMINAL_SEND,
            TERMINAL_SIGNAL,
        ],
        "the registry offers them in one settled order"
    );

    let schemas = registry.schemas();
    let send = schema(&schemas, TERMINAL_SEND);
    assert_eq!(send.parameters["required"], json!(["session_id", "text"]));
    assert!(send.parameters["properties"]["submit"].is_object());
    assert!(
        send.description.contains("[wait: stdin_read]"),
        "the description has to teach the markers the model must read: {}",
        send.description
    );

    let signal = schema(&schemas, TERMINAL_SIGNAL);
    assert_eq!(
        signal.parameters["properties"]["signal"]["enum"],
        json!(["SIGINT", "SIGTERM", "SIGKILL", "SIGTSTP", "SIGHUP"])
    );
    assert_eq!(
        schema(&schemas, TERMINAL_LIST).parameters["properties"],
        json!({})
    );
    assert_eq!(
        schema(&schemas, TERMINAL_OPEN).parameters["properties"]["type"]["enum"],
        json!(["bash"]),
        "the types on offer are the ones this deployment registered"
    );
    for schema in &schemas {
        assert!(
            schema.parameters["properties"]
                .get("run_in_background")
                .is_none(),
            "{} advertises a background mode nothing can collect",
            schema.name
        );
    }
}

/// TC-PORT-TERM-35: a model opens a terminal, types at it twice, and the
/// second command sees what the first one did.
///
/// Upstream: `tool-terminal/tests/tools.spec.ts` ("state persists across
/// sends"), over the loop contract its `agent-loop` suite pins.
///
/// The acceptance criterion of this slice in one case: the tools are only
/// worth anything if the loop dispatches them, records them, and carries their
/// results into the next request. It is also where the two halves meet - the
/// session id the model reads out of one result is the id it must name in the
/// next call.
///
/// Input: a scripted model that opens a terminal, then `cd`s and exports in
/// one send, then reads both back in another.
/// Expected: the second send's result holds the new directory and the
/// variable; the journal records all three calls in order; and the last
/// request carries the tool message the model answered from.
#[tokio::test]
async fn a_model_opens_a_terminal_and_the_next_send_sees_the_last_one() {
    let workspace = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(workspace.path().join("inner")).expect("a directory to change into");
    let script = Script::new(vec![
        Step::Call(call("c1", TERMINAL_OPEN, json!({ "name": "work" }))),
        Step::CallFrom(Box::new(|earlier| {
            call(
                "c2",
                TERMINAL_SEND,
                json!({
                    "session_id": session_id(earlier),
                    "text": "cd inner && export TETANUS_TOOLS=kept",
                }),
            )
        })),
        Step::CallFrom(Box::new(|earlier| {
            call(
                "c3",
                TERMINAL_SEND,
                json!({
                    "session_id": session_id(earlier),
                    "text": "pwd; echo \"var=$TETANUS_TOOLS\"",
                }),
            )
        })),
    ]);
    let harness = Harness::new("terminal-tools", workspace.path(), script.clone()).await;

    harness
        .engine
        .run_turn("open a terminal and use it twice")
        .await
        .expect("the turn ran");

    let results = harness.results();
    assert_eq!(results.len(), 3, "one result per call: {results:#?}");
    let opened = results[0]["content"].as_str().expect("text");
    assert!(
        opened.starts_with("opened terminal session pty-1 (work) [type: bash]"),
        "the open result names the session: {opened:?}"
    );
    let second = results[2]["content"].as_str().expect("text");
    assert!(
        second.contains("/inner") && second.contains("var=kept"),
        "the terminal forgot what the earlier send did: {second:?}"
    );
    assert!(
        second.contains("[wait: stdin_read]") && second.contains("[session: running]"),
        "the markers are what tell the model how the send came back: {second:?}"
    );

    let last = script.request(3).expect("a fourth request");
    assert!(
        last.messages
            .iter()
            .any(|message| message.role == Role::Tool && message.content.contains("var=kept")),
        "the result never reached the next request"
    );
}

/// TC-PORT-TERM-36: a failing command comes back as a failed result carrying
/// its status, and the session stays open.
///
/// Upstream: `isError` for a send, and the exit status its `session.spec.ts`
/// asserts the shell reports.
///
/// A model has to be able to tell "the command failed" from "the tool broke":
/// the first is something to fix in the command, the second is something to
/// report to a human. The marker is how, and the flag agrees with it.
///
/// Input: a send whose command exits non-zero, then another on the same
/// session.
/// Expected: the failing send is recorded as not ok with `[exit code: 3]`, the
/// session is still running, and the next send succeeds.
#[tokio::test]
async fn a_failing_command_is_a_failed_result_and_the_session_stays_open() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let script = Script::new(vec![
        Step::Call(call("c1", TERMINAL_OPEN, json!({}))),
        Step::CallFrom(Box::new(|earlier| {
            call(
                "c2",
                TERMINAL_SEND,
                json!({ "session_id": session_id(earlier), "text": "(exit 3)" }),
            )
        })),
        Step::CallFrom(Box::new(|earlier| {
            call(
                "c3",
                TERMINAL_SEND,
                json!({ "session_id": session_id(earlier), "text": "echo recovered" }),
            )
        })),
    ]);
    let harness = Harness::new("terminal-failure", workspace.path(), script).await;

    harness
        .engine
        .run_turn("run something that fails")
        .await
        .expect("the turn ran");

    let results = harness.results();
    assert_eq!(results[1]["ok"], json!(false), "{:#?}", results[1]);
    let failed = results[1]["content"].as_str().expect("text");
    assert!(
        failed.contains("[exit code: 3]"),
        "the status the shell reported is the whole point: {failed:?}"
    );
    assert!(
        failed.contains("[session: running]"),
        "a failed command does not end the session: {failed:?}"
    );
    assert_eq!(results[2]["ok"], json!(true));
    assert!(results[2]["content"]
        .as_str()
        .expect("text")
        .contains("recovered"));
}

/// TC-PORT-TERM-37: reading a page and listing sessions are safe beside each
/// other, and typing is not.
///
/// Upstream schedules its terminal tools through the same registry as
/// everything else; tetanus's pipeline asks each tool how it may be scheduled,
/// so the answers are the contract.
///
/// A send can start a build; two of them at one terminal interleave into a
/// stream nobody can attribute, so it is a barrier. A read and a list touch
/// nothing outside the process, so making them barriers too would serialize a
/// step for no reason.
///
/// Input: the schedule each tool declares.
/// Expected: `terminal_open`, `terminal_send` and `terminal_close` are
/// exclusive; `terminal_read`, `terminal_list` and `terminal_signal` are
/// parallel.
#[test]
fn typing_is_a_barrier_and_reading_is_not() {
    let (_dir, tools) = terminal_tools(Interrupt::new());
    let registry = tools.registry();
    let mode = |name: &str| registry.mode(&call("c", name, json!({})));

    assert_eq!(mode(TERMINAL_OPEN), ToolMode::Exclusive);
    assert_eq!(mode(TERMINAL_SEND), ToolMode::Exclusive);
    assert_eq!(mode(TERMINAL_CLOSE), ToolMode::Exclusive);
    assert_eq!(mode(TERMINAL_READ), ToolMode::Parallel);
    assert_eq!(mode(TERMINAL_LIST), ToolMode::Parallel);
    assert_eq!(mode(TERMINAL_SIGNAL), ToolMode::Parallel);
}

/// TC-PORT-TERM-38: two parallel-safe calls in one step are answered in the
/// order the model asked for them.
///
/// Upstream: the tool pipeline's ordering guarantee, which its own loop suite
/// pins rather than the terminal package.
///
/// A model reads results positionally: the first result answers the first
/// call. A pipeline that ran two calls at once and committed whichever
/// finished first would hand the model a list where the answers have swapped,
/// and nothing in the text would say so.
///
/// Input: one step asking for `terminal_list` and `terminal_read` together,
/// after a session exists.
/// Expected: both are answered, in the order the model asked, and the read
/// carries its `[lines: …]` marker.
#[tokio::test]
async fn parallel_calls_are_committed_in_model_order() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let script = Script::new(vec![
        Step::Call(call("c1", TERMINAL_OPEN, json!({ "name": "one" }))),
        Step::CallFrom(Box::new(|earlier| {
            call(
                "c2",
                TERMINAL_SEND,
                json!({ "session_id": session_id(earlier), "text": "echo page-me" }),
            )
        })),
        Step::CallsFrom(Box::new(|earlier| {
            vec![
                call("c3", TERMINAL_LIST, json!({})),
                call(
                    "c4",
                    TERMINAL_READ,
                    json!({ "session_id": session_id(earlier), "count": 5 }),
                ),
            ]
        })),
    ]);
    let harness = Harness::new("terminal-order", workspace.path(), script).await;

    harness
        .engine
        .run_turn("look at the session two ways")
        .await
        .expect("the turn ran");

    let results = harness.results();
    assert_eq!(results.len(), 4, "{results:#?}");
    let listed = results[2]["content"].as_str().expect("text");
    assert!(
        listed.contains("pty-1 (one)") && listed.contains("running"),
        "the list is first because the model asked for it first: {listed:?}"
    );
    let page = results[3]["content"].as_str().expect("text");
    assert!(
        page.contains("page-me") && page.contains("[lines: 0-"),
        "the page is second, with its context: {page:?}"
    );
}

/// TC-PORT-TERM-39: a call naming a session nobody opened is refused, and the
/// model is told which mistake it made.
///
/// Upstream: `NO_SESSION` reaching the tool layer as a call failure.
///
/// A model that mistyped an id and a model whose session has been closed need
/// different next moves - retype, or open a new one - and both need to know
/// the call did not happen at all, rather than reading an empty viewport as "a
/// command that printed nothing".
///
/// Input: a send at an id nobody minted, and a close of the same.
/// Expected: both are recorded as failed results naming the id, and the turn
/// carries on.
#[tokio::test]
async fn a_call_at_an_unknown_session_is_refused_in_words() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let script = Script::new(vec![
        Step::Call(call(
            "c1",
            TERMINAL_SEND,
            json!({ "session_id": "pty-404", "text": "echo hello" }),
        )),
        Step::Call(call(
            "c2",
            TERMINAL_CLOSE,
            json!({ "session_id": "pty-404" }),
        )),
    ]);
    let harness = Harness::new("terminal-unknown", workspace.path(), script).await;

    harness
        .engine
        .run_turn("use a session that is not there")
        .await
        .expect("the turn ran");

    let results = harness.results();
    assert_eq!(results.len(), 2, "{results:#?}");
    for result in &results {
        assert_eq!(result["ok"], json!(false));
        assert!(
            result["content"]
                .as_str()
                .expect("text")
                .contains("pty-404"),
            "the refusal has to name the id: {result:#?}"
        );
    }
}

/// TC-PORT-TERM-40: a stopped turn interrupts the command the terminal is
/// running, and the session survives to be closed.
///
/// Upstream: `exec.signal` reaching its send, which cancels with `SIGINT`.
///
/// The pipe-backed `shell_run` ends its whole session when a turn is stopped,
/// because a shell reading a pipe cannot be interrupted; a terminal can, so a
/// stopped turn costs the command and not the session. That difference is the
/// reason this family exists beside the other one, and it is only true if the
/// tools hold the *turn's* switch rather than one of their own.
///
/// Input: a session, then a send of a long sleep, with the turn stopped while
/// it runs.
/// Expected: the send comes back reporting the interrupt, the session is still
/// running, and its shell is gone once the session is closed.
#[tokio::test]
async fn a_stopped_turn_interrupts_the_command_and_leaves_the_session() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let interrupt = Interrupt::new();
    let (dir, tools) = terminal_tools_in(workspace.path(), Arc::clone(&interrupt));
    let session = tools
        .terminals()
        .open(&Owner::new("session"), Default::default())
        .await
        .expect("a terminal");
    let registry = tools.registry();
    let send = call("c1", TERMINAL_SEND, json!({}));

    let thrown = Arc::clone(&interrupt);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        thrown.stop();
    });
    let outcome = registry
        .execute(&ToolCall {
            arguments: json!({ "session_id": session.id(), "text": "sleep 30" }),
            ..send
        })
        .await
        .expect("the call answered");

    assert!(
        outcome.content.contains("[wait: interrupted]"),
        "the send should say the turn stopped it: {:?}",
        outcome.content
    );
    assert!(
        outcome.content.contains("[session: running]"),
        "the session survives its own interrupt: {:?}",
        outcome.content
    );
    let pid = session.pid();
    tools.terminals().close_all().await;
    assert!(
        !alive(pid),
        "the shell outlived the session that was closed"
    );
    drop(dir);
}

/// TC-PORT-TERM-41: long work is started and collected later, which is what a
/// terminal has instead of a background job.
///
/// Upstream's answer to "run the build and come back" is `run_in_background`,
/// which returns a job id that `job_output` and `job_kill` collect - one
/// feature with a job store behind it, and tetanus has no store (`jobs/*` in
/// `docs/parity.md`). A terminal needs none: the session *is* the collection
/// point. A send that stops waiting leaves the command running on the
/// terminal, a read collects what it printed since, and a signal stops it.
///
/// `wait_ms` is what makes that deliberate rather than accidental. Without it
/// a model could only get the same behaviour by hoping the deployment's
/// timeout was short, and would read `[wait: timeout]` as a fault rather than
/// as the answer it asked for.
///
/// Input: a session; a send of a slow counting loop with a small `wait_ms`;
/// then a read; then a signal; then a read.
/// Expected: the send returns promptly saying it did not wait for the end and
/// the session is still running; the first read shows the command had
/// progressed; the signal reaches it; and the second read shows more output
/// than the first, then no more.
#[tokio::test]
async fn long_work_is_started_and_collected_later() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let interrupt = Interrupt::new();
    let (_dir, tools) = terminal_tools_in(workspace.path(), interrupt);
    let session = tools
        .terminals()
        .open(&Owner::new("session"), Default::default())
        .await
        .expect("a terminal");
    let registry = tools.registry();
    let id = session.id().to_string();

    let started = std::time::Instant::now();
    let outcome = registry
        .execute(&call(
            "c1",
            TERMINAL_SEND,
            json!({
                "session_id": id,
                "text": "for i in $(seq 1 100); do echo tick-$i; sleep 0.1; done",
                "wait_ms": 400,
            }),
        ))
        .await
        .expect("the call answered");

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a short wait must come back short, not wait for the command"
    );
    assert!(
        outcome.content.contains("[wait: timeout]"),
        "the send says it stopped waiting rather than that the command ended: {:?}",
        outcome.content
    );
    assert!(
        outcome.content.contains("[session: running]"),
        "the work is still going on the terminal: {:?}",
        outcome.content
    );

    let first = read_page(&registry, &id).await;
    assert!(
        first.contains("tick-"),
        "the command was running while nobody waited: {first:?}"
    );

    tokio::time::sleep(Duration::from_millis(500)).await;
    let second = read_page(&registry, &id).await;
    assert!(
        ticks(&second) > ticks(&first),
        "it should have gone on printing: {} then {}",
        ticks(&first),
        ticks(&second)
    );

    let stopped = registry
        .execute(&call(
            "c2",
            TERMINAL_SIGNAL,
            json!({ "session_id": id, "signal": "SIGINT" }),
        ))
        .await
        .expect("the call answered");
    assert!(
        stopped.content.contains("delivered SIGINT"),
        "the signal is how a caller stops what it started: {:?}",
        stopped.content
    );

    let after = read_page(&registry, &id).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let later = read_page(&registry, &id).await;
    assert_eq!(
        ticks(&after),
        ticks(&later),
        "the command was stopped, so nothing more should arrive"
    );
    tools.terminals().close_all().await;
}

// ---------------------------------------------------------------- fixtures

/// The whole retained page of one session, through the tool a model would use.
async fn read_page(registry: &tetanus_turn::tools::ToolRegistry, id: &str) -> String {
    registry
        .execute(&call(
            "read",
            TERMINAL_READ,
            json!({ "session_id": id, "count": 500 }),
        ))
        .await
        .expect("the call answered")
        .content
}

/// How many ticks a page holds, which is how a case measures progress without
/// depending on how fast the machine is.
fn ticks(page: &str) -> usize {
    page.matches("tick-").count()
}

/// A step written against what the earlier results said, which is how a case
/// names the session the model opened one step ago.
type FromEarlier<T> = Box<dyn Fn(&[String]) -> T + Send + Sync>;

/// One step of the scripted model.
enum Step {
    Call(ToolCall),
    CallFrom(FromEarlier<ToolCall>),
    CallsFrom(FromEarlier<Vec<ToolCall>>),
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
        let earlier: Vec<String> = request
            .messages
            .iter()
            .filter(|message| message.role == Role::Tool)
            .map(|message| message.content.clone())
            .collect();

        let calls: Vec<ToolCall> = match self.steps.get(index) {
            Some(Step::Call(call)) => vec![call.clone()],
            Some(Step::CallFrom(make)) => vec![make(&earlier)],
            Some(Step::CallsFrom(make)) => make(&earlier),
            None => Vec::new(),
        };

        let content = if calls.is_empty() {
            format!("done: {}", earlier.last().cloned().unwrap_or_default())
        } else {
            "at the terminal now".to_string()
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

/// One booted engine with the terminal tools on it, writing to a temp journal.
struct Harness {
    engine: TurnEngine,
    log: Arc<dyn SessionLog>,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new(name: &str, workspace: &std::path::Path, script: Script) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let bus = EventBus::new();
        let log: Arc<dyn SessionLog> =
            JsonlSessionLog::create(name, dir.path().join(format!("{name}.jsonl")), bus.clone())
                .expect("journal");
        let interrupt = Interrupt::new();
        let (_tools_dir, tools) = terminal_tools_in(workspace, Arc::clone(&interrupt));
        let ctx = boot_with(
            bus,
            Arc::new(script),
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

/// The terminal tools over a bash backend in a fresh workspace.
fn terminal_tools(interrupt: Arc<Interrupt>) -> (tempfile::TempDir, Arc<TerminalTools>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let tools = terminal_tools_in(dir.path(), interrupt).1;
    (dir, tools)
}

/// The terminal tools rooted at `workspace`, with budgets short enough that a
/// case waiting for one waits for milliseconds.
fn terminal_tools_in(
    workspace: &std::path::Path,
    interrupt: Arc<Interrupt>,
) -> (std::path::PathBuf, Arc<TerminalTools>) {
    let terminals = Terminals::with(
        TerminalConfig {
            cwd: workspace.to_path_buf(),
            idle_silence: Duration::from_secs(5),
            timeout: Duration::from_secs(20),
            grace: Duration::from_millis(200),
            ..TerminalConfig::default()
        },
        Arc::new(Bash::new()),
    )
    .expect("one backend registers");
    (
        workspace.to_path_buf(),
        TerminalTools::new(Arc::new(terminals), Owner::new("session"), interrupt),
    )
}

/// One tool call, as a model writes one.
fn call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
    }
}

/// The session id an earlier `terminal_open` result announced.
fn session_id(earlier: &[String]) -> String {
    let opened = earlier
        .iter()
        .find(|text| text.starts_with("opened terminal session "))
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
