//! Test Design Specification: a one-shot command that outlives the call that
//! started it.
//!
//! Features under test: `tetanus_exec::background` and the two tool surfaces
//! over it - `shell`'s `run_in_background` argument and the `shell_result`
//! tool that collects what it started. Upstream pins the producing half in
//! `packages/shell/tool-bash/tests/tools.spec.ts` (`run_in_background`), whose
//! collection surface is its own `BashOutput` tool.
//!
//! Approach: real commands, a real job store on disk and a real artifact
//! directory. A backgrounded command is only interesting because of what
//! happens *after* the call returns, so every case here starts one and then
//! observes it from outside: the store, the artifact file, and a second tool
//! call. Nothing here inspects a future or a task handle, because a job a
//! caller can only see by holding the object that made it is not the thing
//! this feature promises.
//!
//! What is not restated, and why. Upstream's `BashOutput` filters by regex and
//! consumes an output cursor per read; both belong to a producer that streams,
//! which `tetanus_core::jobs` deliberately is not - it keeps a producer's final
//! output and not a stream - so the artifact is the stream and a read of it is
//! whole rather than a delta. Killing a backgrounded job has no tool here: the
//! kill surface belongs with the rest of the job vocabulary in `tool-jobs`,
//! which the `workflow/*`, `schedule/*`, `jobs/*` row owns.
//!
//! The convention these cases pin - where the output goes and where its path
//! is recorded - is contract section 4.3.6, marked provisional there because
//! the store belongs to that other row.
//!
//! Environmental needs: a bash on PATH, a writable temp directory. No case
//! reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tetanus_core::jobs::{JobStatus, JobStore};
use tetanus_core::spill::SpillStore;
use tetanus_exec::backend::Bash;
use tetanus_exec::background::{artifact_of, BackgroundTo};
use tetanus_exec::session::SessionConfig;
use tetanus_exec::shell::ShellConfig;
use tetanus_exec::tools::{ShellTools, SHELL, SHELL_RESULT};
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::tools::{ToolOutcome, ToolRegistry};

/// TC-PORT-SHELL-22: a backgrounded command answers before it has finished,
/// with an id a later call can use.
///
/// Upstream: "returns immediately with a task id when run_in_background is
/// set".
///
/// This is the whole point of the argument. A model backgrounds a build so it
/// can do something else, so an answer that arrives when the build does is the
/// feature not working, however correct the output is.
///
/// Input: `shell` with `run_in_background: true` on a command that sleeps for
/// two seconds and then prints.
/// Expected: the call answers in well under that; the answer names a job id;
/// the store holds that job as live; and the command is still running.
#[tokio::test]
async fn a_backgrounded_command_answers_before_it_has_finished() {
    let ground = Ground::new();
    let started = std::time::Instant::now();
    let answer = ground
        .call(
            SHELL,
            json!({ "command": "sleep 2; echo done", "run_in_background": true }),
        )
        .await;

    assert!(
        started.elapsed() < Duration::from_millis(1_500),
        "the call waited for the command: {:?}",
        started.elapsed()
    );
    let id = job_id(&answer);
    let job = ground.jobs.get(&id).expect("the store holds the job");
    assert!(
        job.is_live(),
        "a job that answered immediately should still be live, not {:?}",
        job.status
    );
    assert_eq!(job.kind, "shell");
    assert_eq!(job.label, "sleep 2; echo done", "the label is the command");
}

/// TC-PORT-SHELL-23: what a backgrounded command printed is collectable once
/// it is over.
///
/// Upstream: the `BashOutput` half - "reads the output of a background task".
///
/// A producer with no consumer is a capability nobody can reach, so the tool
/// that starts the work answers for it too, until the jobs row's own tools
/// exist.
///
/// Input: a backgrounded `echo`, then `shell_result` with its id once the job
/// is terminal.
/// Expected: the collected text holds what the command printed, and says the
/// job finished.
#[tokio::test]
async fn what_a_backgrounded_command_printed_is_collectable_when_it_is_over() {
    let ground = Ground::new();
    let answer = ground
        .call(
            SHELL,
            json!({ "command": "echo hello-from-the-background", "run_in_background": true }),
        )
        .await;
    let id = job_id(&answer);
    ground.settled(&id).await;

    let collected = ground.call(SHELL_RESULT, json!({ "job": id })).await;
    let text = text_of(&collected);
    assert!(
        text.contains("hello-from-the-background"),
        "the collection did not carry the output: {text}"
    );
    assert!(
        text.contains("finished"),
        "the collection did not say the job was over: {text}"
    );
}

/// TC-PORT-SHELL-24: the record says where the complete output is, in the
/// shape contract section 4.3.6 fixes.
///
/// The store keeps a producer's *final* output and explicitly not a stream, so
/// a backgrounded command's whole stream lives in a spill artifact beside the
/// session's journal and the record names it. This case pins both halves,
/// because the convention is the part another lane will inherit: a reader that
/// found the artifact by guessing a filename would keep working while the
/// convention rotted.
///
/// Input: a backgrounded command printing more than one line, run to
/// completion.
/// Expected: `detail` parses as JSON carrying `artifact`; that file exists and
/// holds every line the command printed; and the store's own `output` field
/// carries the rendered result.
#[tokio::test]
async fn the_record_names_the_artifact_and_keeps_the_final_output() {
    let ground = Ground::new();
    let answer = ground
        .call(
            SHELL,
            json!({ "command": "echo one; echo two; echo three", "run_in_background": true }),
        )
        .await;
    let id = job_id(&answer);
    ground.settled(&id).await;

    let job = ground.jobs.get(&id).expect("the store holds the job");
    assert_eq!(job.status, JobStatus::Completed);

    let detail = job
        .detail
        .as_deref()
        .expect("a finished job carries detail");
    let parsed: Value =
        serde_json::from_str(detail).expect("detail is the JSON object 4.3.6 fixes");
    let artifact = parsed["artifact"]
        .as_str()
        .expect("`artifact` is the one key that section fixes")
        .to_string();
    assert_eq!(
        artifact_of(Some(detail)).as_deref(),
        Some(artifact.as_str()),
        "the reader in the crate and the shape on the record must agree"
    );

    let whole = std::fs::read_to_string(&artifact).expect("the artifact is a readable file");
    for line in ["one", "two", "three"] {
        assert!(
            whole.contains(line),
            "the artifact is meant to hold the complete stream, and `{line}` is missing: {whole}"
        );
    }

    let output = job.output.as_deref().expect("the store's own output field");
    assert!(
        output.contains("one") && output.contains("three"),
        "the final output a foreground call would have answered with: {output}"
    );
}

/// TC-PORT-SHELL-25: a job still running is readable, and says so.
///
/// A collection that only worked once the job was over would make a model
/// wait for the thing it backgrounded in order to avoid waiting for it, and
/// the answer to "how far has the build got" is exactly what a background run
/// is for.
///
/// Input: a command that prints, then sleeps, then prints again; collected
/// while it is between the two.
/// Expected: the collection carries the first line, does not carry the second,
/// and says the job is still running.
#[tokio::test]
async fn a_job_still_running_is_readable_and_says_so() {
    let ground = Ground::new();
    let answer = ground
        .call(
            SHELL,
            json!({ "command": "echo first; sleep 3; echo second", "run_in_background": true }),
        )
        .await;
    let id = job_id(&answer);

    // Wait for the first line to reach the artifact rather than for a fixed
    // interval: the assertion is about what has been written, so the wait is
    // for that fact and not for a duration that a loaded box makes a lie.
    let text = ground.until_collected_contains(&id, "first").await;
    assert!(
        !printed(&text).contains("second"),
        "the command cannot have printed its second line yet: {text}"
    );
    assert!(
        text.contains("still running"),
        "a live job has to say it is live: {text}"
    );
}

/// TC-PORT-SHELL-26: a deployment with no job store refuses to background a
/// command instead of running it in the foreground.
///
/// The dangerous answer is the helpful one. A tool that quietly ran the
/// command in the foreground would block the step a model deliberately tried
/// not to block, and would answer with output the model was not waiting for,
/// with nothing saying the mode it asked for was unavailable.
///
/// Input: shell tools composed without `backgrounding`, called with
/// `run_in_background: true`.
/// Expected: an error naming what is missing; the command does not run.
#[tokio::test]
async fn a_deployment_with_no_store_refuses_to_background_a_command() {
    let ground = Ground::without_store();
    let marker = ground.workspace.path().join("must-not-exist");
    let error = ground
        .registry
        .get(SHELL)
        .expect("the tool is registered")
        .execute(&json!({
            "command": format!("touch {}", marker.display()),
            "run_in_background": true,
        }))
        .await
        .expect_err("a background run with nowhere to record it must be refused");

    let said = error.to_string();
    assert!(
        said.contains("job store"),
        "the refusal has to name what is missing: {said}"
    );
    assert!(
        !marker.exists(),
        "the command must not have run: a refusal that ran it anyway is worse than either answer"
    );
}

/// TC-PORT-SHELL-27: stopping the turn does not kill a backgrounded command.
///
/// This is the clause that makes `run_in_background` mean what it says. A
/// foreground command holds the turn's interrupt and is swept when the turn
/// stops, which is right: it is the turn's command. A backgrounded one is not,
/// and sweeping it would make the argument mean "until the user presses stop" -
/// so a model backgrounding a twenty-minute test suite would lose it to an
/// unrelated interrupt, silently.
///
/// Input: a backgrounded command that writes a file after a delay; the turn's
/// interrupt fired immediately afterwards.
/// Expected: the file appears anyway, and the job reaches `Completed`.
#[tokio::test]
async fn stopping_the_turn_does_not_kill_a_backgrounded_command() {
    let ground = Ground::new();
    let marker = ground.workspace.path().join("survivor");
    let answer = ground
        .call(
            SHELL,
            json!({
                "command": format!("sleep 1; touch {}", marker.display()),
                "run_in_background": true,
            }),
        )
        .await;
    let id = job_id(&answer);

    ground.interrupt.stop();

    ground.settled(&id).await;
    assert!(
        marker.exists(),
        "the interrupt killed a command that was no longer the turn's"
    );
    assert_eq!(
        ground.jobs.get(&id).expect("the job").status,
        JobStatus::Completed
    );
}

/// TC-PORT-SHELL-28: the collection tool is registered and advertises what it
/// needs.
///
/// A capability the model cannot see is one it will never use, and this is the
/// only route to a backgrounded command's output until the jobs row's tools
/// exist.
///
/// Input: the registry the shell tools register on.
/// Expected: `shell_result` is offered, requires `job`, and its description
/// points back at the argument that starts one.
#[test]
fn the_collection_tool_is_registered_and_advertises_what_it_needs() {
    let ground = Ground::new();
    let schemas = ground.registry.schemas();
    let result = schemas
        .iter()
        .find(|schema| schema.name == SHELL_RESULT)
        .expect("the collection tool is registered");
    assert_eq!(result.parameters["required"], json!(["job"]));
    assert!(
        result.description.contains(SHELL),
        "a model reading this has to learn where a job id comes from: {}",
        result.description
    );

    let shell = schemas
        .iter()
        .find(|schema| schema.name == SHELL)
        .expect("the one-shot tool is registered");
    assert!(
        shell.parameters["properties"]
            .get("run_in_background")
            .is_some(),
        "the argument is not advertised, so no model will send it"
    );
    assert!(
        shell.description.contains(SHELL_RESULT)
            || shell.parameters["properties"]["run_in_background"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains(SHELL_RESULT),
        "the argument has to name the tool that collects what it starts"
    );
}

/// Everything a case needs: a workspace, a store, an artifact directory and
/// the tools composed over them.
struct Ground {
    workspace: tempfile::TempDir,
    #[allow(dead_code)]
    artifacts: tempfile::TempDir,
    jobs: Arc<JobStore>,
    registry: ToolRegistry,
    interrupt: Arc<Interrupt>,
}

impl Ground {
    fn new() -> Self {
        let workspace = tempfile::tempdir().expect("workspace");
        let artifacts = tempfile::tempdir().expect("artifacts");
        let jobs =
            Arc::new(JobStore::open(artifacts.path().join("jobs.jsonl")).expect("a job store"));
        let interrupt = Interrupt::new();
        let tools = shell_tools(workspace.path(), Arc::clone(&interrupt));
        tools.backgrounding(BackgroundTo {
            jobs: Arc::clone(&jobs),
            spill: Arc::new(SpillStore::at(artifacts.path().join("artifacts"))),
            session: "session-under-test".to_string(),
        });
        Self {
            registry: tools.registry(),
            workspace,
            artifacts,
            jobs,
            interrupt,
        }
    }

    /// The same composition with nothing attached, which is what a deployment
    /// that never wired a store has.
    fn without_store() -> Self {
        let workspace = tempfile::tempdir().expect("workspace");
        let artifacts = tempfile::tempdir().expect("artifacts");
        let jobs =
            Arc::new(JobStore::open(artifacts.path().join("jobs.jsonl")).expect("a job store"));
        let interrupt = Interrupt::new();
        let tools = shell_tools(workspace.path(), Arc::clone(&interrupt));
        Self {
            registry: tools.registry(),
            workspace,
            artifacts,
            jobs,
            interrupt,
        }
    }

    async fn call(&self, tool: &str, arguments: Value) -> ToolOutcome {
        self.registry
            .get(tool)
            .unwrap_or_else(|| panic!("`{tool}` is registered"))
            .execute(&arguments)
            .await
            .unwrap_or_else(|error| panic!("`{tool}` refused: {error}"))
    }

    /// Wait until the job is terminal, bounded so a wedged case fails rather
    /// than hangs.
    async fn settled(&self, id: &str) {
        for _ in 0..600 {
            match self.jobs.get(id) {
                Some(job) if job.status.is_terminal() => return,
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        panic!("job `{id}` never finished");
    }

    /// Collect until the command's own output holds `needle`, and answer with
    /// that whole collection.
    async fn until_collected_contains(&self, id: &str, needle: &str) -> String {
        for _ in 0..100 {
            let text = text_of(&self.call(SHELL_RESULT, json!({ "job": id })).await);
            if printed(&text).contains(needle) {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("`{needle}` never reached the collection for job `{id}`");
    }
}

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
    .expect("bash is on PATH")
}

/// What the command printed, without the status line the collection appends.
///
/// The status line quotes the command as its label, so a command whose own
/// text contains the word being looked for would satisfy an assertion about
/// its *output* before it had printed anything. The first cut of
/// TC-PORT-SHELL-25 had exactly that bug and passed the wrong half of it.
fn printed(collection: &str) -> &str {
    match collection.rfind("[job ") {
        Some(at) => &collection[..at],
        None => collection,
    }
}

fn text_of(outcome: &ToolOutcome) -> String {
    outcome.content.clone()
}

/// Read the job id out of the answer a backgrounded call gives.
///
/// Parsed from the text a model reads rather than from a side channel,
/// because the text is the only thing the model gets: an id it cannot find
/// there is an id it cannot use.
fn job_id(outcome: &ToolOutcome) -> String {
    let text = text_of(outcome);
    let start = text.find("[job ").expect("the answer names the job") + "[job ".len();
    let rest = &text[start..];
    let end = rest.find(':').expect("the id is followed by its state");
    rest[..end].to_string()
}
