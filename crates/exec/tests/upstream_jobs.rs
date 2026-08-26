//! Test Design Specification: a command that outlives the turn that asked for it.
//!
//! Feature under test: `run_in_background` on the `shell` tool, and the three
//! tools that make a background command collectable - `job_list`,
//! `job_output`, `job_kill` - over the durable job store in `tetanus_core`.
//!
//! Why this exists: the store landed with no caller. The process lane closed
//! its own row leaving exactly this clause open and declined to build a second
//! store for it, saying "the wiring is a `run_in_background` argument on the
//! existing tool that starts the same `proc::Command` without awaiting it and
//! hands the store the group id". This is that wiring, minus the group id -
//! see the note for why the kill is the turn's switch for now.
//!
//! Approach: real commands through a real registry against a real store on
//! disk. A background command is only interesting because it outlasts the call
//! that started it, and a mock cannot outlast anything.
//!
//! Environmental needs: a writable temporary directory, a Tokio runtime and a
//! POSIX shell. No network, no credential.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tetanus_core::jobs::{JobStatus, JobStore};
use tetanus_exec::backend::Bash;
use tetanus_exec::session::SessionConfig;
use tetanus_exec::shell::ShellConfig;
use tetanus_exec::tools::{ShellTools, JOB_KILL, JOB_LIST, JOB_OUTPUT, SHELL};
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::tools::{ToolCall, ToolOutcome, ToolRegistry};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<JobStore>,
    registry: ToolRegistry,
}

fn fixture(session: Option<&str>) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(JobStore::open(dir.path().join("jobs.jsonl")).expect("a job store"));
    let tools = ShellTools::new(
        Arc::new(Bash::new()),
        ShellConfig {
            cwd: dir.path().to_path_buf(),
            ..ShellConfig::default()
        },
        SessionConfig::default(),
        Interrupt::new(),
    )
    .expect("bash is on this host");
    let tools = tools.with_jobs(Arc::clone(&store), session.map(str::to_owned));
    let mut registry = ToolRegistry::new();
    tools.register(&mut registry);
    Fixture {
        _dir: dir,
        store,
        registry,
    }
}

async fn run(registry: &ToolRegistry, name: &str, arguments: serde_json::Value) -> ToolOutcome {
    registry
        .execute(&ToolCall {
            id: format!("call-{name}"),
            name: name.to_string(),
            arguments,
        })
        .await
        .expect("the tool answered rather than failing the step")
}

/// The id out of `started ... as job <id>.`
fn job_id(said: &str) -> String {
    said.split("as job ")
        .nth(1)
        .and_then(|rest| rest.split('.').next())
        .unwrap_or_else(|| panic!("no job id in: {said}"))
        .trim()
        .to_string()
}

/// Wait for a job to reach a terminal state, or give up loudly.
async fn settled(store: &JobStore, id: &str) -> tetanus_core::jobs::Job {
    for _ in 0..200 {
        if let Some(job) = store.get(id) {
            if !job.is_live() {
                return job;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("job {id} never settled");
}

/// TC-PORT-JOB-13: a background command answers at once with an id, and the
/// record exists before the work does.
///
/// Input: `shell` with `run_in_background`, running a command that sleeps.
/// Expected: the call returns immediately, naming a job id and how to collect
/// it; the store already holds that job, live. The order is the point - a job
/// that started with no record is work nobody can find, while a record whose
/// work never started is one `job_output` away from saying so.
#[tokio::test]
async fn a_background_command_answers_with_an_id_and_a_record_that_already_exists() {
    let f = fixture(Some("session-a"));

    let started = run(
        &f.registry,
        SHELL,
        json!({ "command": "sleep 0.4; echo done", "run_in_background": true }),
    )
    .await;

    assert!(started.ok, "{}", started.content);
    let id = job_id(&started.content);
    let job = f
        .store
        .get(&id)
        .expect("the record is written before the work");
    assert!(job.is_live(), "{job:?}");
    assert!(started.content.contains(JOB_OUTPUT), "{}", started.content);
}

/// TC-PORT-JOB-14: the output is collectable after the turn that asked has
/// moved on, and a job still running says so rather than failing.
///
/// Input: a background command; `job_output` immediately, then after it ends.
/// Expected: the first read is `ok` and says the job is running with nothing to
/// read; the second carries what the command printed. "Still running" is an
/// answer a model acts on, not an error it retries.
#[tokio::test]
async fn output_is_collected_later_and_a_running_job_says_so() {
    let f = fixture(Some("session-a"));
    let started = run(
        &f.registry,
        SHELL,
        json!({ "command": "sleep 0.3; echo collected", "run_in_background": true }),
    )
    .await;
    let id = job_id(&started.content);

    let early = run(&f.registry, JOB_OUTPUT, json!({ "job_id": id })).await;
    let job = settled(&f.store, &id).await;
    let late = run(&f.registry, JOB_OUTPUT, json!({ "job_id": id })).await;

    assert!(early.ok, "a running job is not a failed read");
    assert!(
        early.content.contains("Nothing to read yet"),
        "{}",
        early.content
    );
    assert_eq!(job.status, JobStatus::Completed, "{job:?}");
    assert!(late.content.contains("collected"), "{}", late.content);
}

/// TC-PORT-JOB-15: a command that fails is a completed read of a failed job.
///
/// Input: a background command exiting non-zero.
/// Expected: the job is `Failed`, and `job_output` answers `ok` carrying the
/// exit marker - the same rule the foreground tool follows, where a non-zero
/// exit is news rather than a tool failure.
#[tokio::test]
async fn a_failed_command_is_a_successful_read_of_a_failed_job() {
    let f = fixture(Some("session-a"));
    let started = run(
        &f.registry,
        SHELL,
        json!({ "command": "echo bad 1>&2; exit 3", "run_in_background": true }),
    )
    .await;
    let id = job_id(&started.content);

    let job = settled(&f.store, &id).await;
    let read = run(&f.registry, JOB_OUTPUT, json!({ "job_id": id })).await;

    assert_eq!(job.status, JobStatus::Failed, "{job:?}");
    assert!(read.ok, "reading a failed job is not itself a failure");
    assert!(read.content.contains("exit code: 3"), "{}", read.content);
}

/// TC-PORT-JOB-16: the list is this session's, and another session's job is
/// neither listed nor readable.
///
/// Input: one store, two compositions naming different sessions.
/// Expected: each lists only its own, and reading the other's id is refused by
/// name. Two sessions share one file, and a job id is guessable.
#[tokio::test]
async fn a_session_sees_only_its_own_jobs() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(JobStore::open(dir.path().join("jobs.jsonl")).expect("store"));
    let make = |session: &str| {
        let tools = ShellTools::new(
            Arc::new(Bash::new()),
            ShellConfig {
                cwd: dir.path().to_path_buf(),
                ..ShellConfig::default()
            },
            SessionConfig::default(),
            Interrupt::new(),
        )
        .expect("bash")
        .with_jobs(Arc::clone(&store), Some(session.to_string()));
        let mut registry = ToolRegistry::new();
        tools.register(&mut registry);
        registry
    };
    let mine = make("session-a");
    let theirs = make("session-b");

    let started = run(
        &mine,
        SHELL,
        json!({ "command": "echo mine", "run_in_background": true }),
    )
    .await;
    let id = job_id(&started.content);
    let my_list = run(&mine, JOB_LIST, json!({})).await;
    let their_list = run(&theirs, JOB_LIST, json!({})).await;
    let their_read = run(&theirs, JOB_OUTPUT, json!({ "job_id": id })).await;

    assert!(my_list.content.contains(&id), "{}", my_list.content);
    assert!(
        their_list.content.contains("no background commands"),
        "{}",
        their_list.content
    );
    assert!(!their_read.ok);
    assert!(
        their_read.content.contains("has no job"),
        "{}",
        their_read.content
    );
}

/// TC-PORT-JOB-17: a job can be stopped, and stopping one that has ended is
/// not an error.
///
/// Input: a long background command, killed; then a killed job killed again.
/// Expected: the record reads `Cancelled` with the reason; the second call is
/// `ok` and says it had already ended. A tool that failed on the second call
/// would make a retry after a dropped connection look like a defect.
#[tokio::test]
async fn a_job_is_stopped_and_stopping_a_finished_one_is_not_an_error() {
    let f = fixture(Some("session-a"));
    let started = run(
        &f.registry,
        SHELL,
        json!({ "command": "sleep 30", "run_in_background": true }),
    )
    .await;
    let id = job_id(&started.content);

    let killed = run(&f.registry, JOB_KILL, json!({ "job_id": id })).await;
    let again = run(&f.registry, JOB_KILL, json!({ "job_id": id })).await;

    assert!(killed.ok, "{}", killed.content);
    assert_eq!(
        f.store.get(&id).expect("the job").status,
        JobStatus::Cancelled
    );
    assert!(again.ok, "{}", again.content);
    assert!(again.content.contains("already ended"), "{}", again.content);
}

/// TC-PORT-JOB-18: with no store composed, the job tools are still offered and
/// every one of them explains itself.
///
/// The first cut of this slice registered them only with a store, and running
/// the binary showed the cost: `tetanus tools` composes no session and so no
/// store, so the catalogue advertised five tools where a run offers eight -
/// the disagreement contract §4.7.3 forbids, and one a client cannot tell from
/// an empty toolbox.
///
/// Input: the shell tools with no store.
/// Expected: the roster carries all three job tools; a background call and a
/// `job_list` both fail with a sentence naming what is missing. A model told
/// "this build keeps no job records" runs the command in the foreground; a
/// model whose tool vanished between the catalogue and the call learns
/// nothing.
#[tokio::test]
async fn without_a_store_there_are_no_job_tools_and_backgrounding_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let tools = ShellTools::new(
        Arc::new(Bash::new()),
        ShellConfig {
            cwd: dir.path().to_path_buf(),
            ..ShellConfig::default()
        },
        SessionConfig::default(),
        Interrupt::new(),
    )
    .expect("bash");
    let mut registry = ToolRegistry::new();
    tools.register(&mut registry);

    let names: Vec<String> = registry.schemas().into_iter().map(|s| s.name).collect();
    let refused = run(
        &registry,
        SHELL,
        json!({ "command": "echo hi", "run_in_background": true }),
    )
    .await;

    assert_eq!(
        names.iter().filter(|name| name.starts_with("job_")).count(),
        3,
        "the catalogue and a run offer one set: {names:?}"
    );
    assert!(!refused.ok);
    assert!(
        refused.content.contains("keeps no job records"),
        "{}",
        refused.content
    );
    let listed = run(&registry, JOB_LIST, json!({})).await;
    assert!(!listed.ok);
    assert!(
        listed.content.contains("keeps no job records"),
        "{}",
        listed.content
    );
}
