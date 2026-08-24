//! Test Design Specification: the durable job store, ported.
//!
//! Features under test: `tetanus_core::jobs` - the lifecycle a job record
//! moves through, what survives a restart, and what a reopen does with work
//! the process was doing when it died. Upstream pins the registry half in
//! `packages/jobs/jobs/tests/service.spec.ts` and
//! `jobs-local/tests/jobs.spec.ts`.
//!
//! Approach: a journal in a temporary directory, driven through the public
//! seam, and a "restart" modelled the only honest way - dropping the store and
//! opening the same file again, so the second store learns everything from
//! disk and nothing from memory.
//!
//! What is not restated, and why. Upstream's registry is in memory and its
//! durability is the owning agent's session; persistence is the tetanus
//! difference, so its disposal, reentrancy and agent-teardown cases have no
//! counterpart and its `reported` flag - which suppresses a duplicate
//! completion notice to a live reader - is a reporting concern rather than a
//! storage one. Its `readOutput` consuming cursor belongs to a producer that
//! streams; this stores the terminal output a producer hands over.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tetanus_core::jobs::{JobError, JobStatus, JobStore};

fn store() -> (JobStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JobStore::open(dir.path().join("jobs.jsonl")).expect("open");
    (store, dir)
}

/// TC-PORT-JOB-1: a job moves queued, running, then to exactly one end.
///
/// Upstream: `service.spec.ts`, "tracks a job through its lifecycle".
///
/// Expected: each transition is reflected, the timestamps appear in order, and
/// the terminal record carries the producer's detail and output.
#[test]
fn a_job_moves_through_its_lifecycle() {
    let (store, _dir) = store();

    let queued = store
        .queue(None, "bash", "cargo test --workspace", Some("s-1"))
        .expect("queue");
    assert_eq!(queued.status, JobStatus::Queued);
    assert_eq!(queued.kind, "bash");
    assert_eq!(queued.session.as_deref(), Some("s-1"));
    assert!(queued.started_at.is_none());

    let running = store.start(&queued.id).expect("start");
    assert_eq!(running.status, JobStatus::Running);
    assert!(running.started_at.is_some());
    assert!(running.is_live());

    let done = store
        .finish(
            &queued.id,
            JobStatus::Completed,
            Some("exit code: 0"),
            Some("1037 passed"),
        )
        .expect("finish");
    assert_eq!(done.status, JobStatus::Completed);
    assert_eq!(done.detail.as_deref(), Some("exit code: 0"));
    assert_eq!(done.output.as_deref(), Some("1037 passed"));
    assert!(!done.is_live());
    assert!(done.finished_at.is_some());
    assert!(done.finished_at >= done.started_at);
}

/// TC-PORT-JOB-2: a settled job survives a restart exactly as it settled.
///
/// The claim persistence exists for. A second store over the same file learns
/// everything from disk.
///
/// Expected: the reopened record equals the one the first store answered.
#[test]
fn a_settled_job_survives_a_restart() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("jobs.jsonl");

    let settled = {
        let store = JobStore::open(&path).expect("open");
        let job = store.queue(Some("build-7"), "bash", "make", None).unwrap();
        store.start(&job.id).unwrap();
        store
            .finish(&job.id, JobStatus::Failed, Some("exit code: 2"), None)
            .unwrap()
    };

    let restarted = JobStore::open(&path).expect("reopen");
    assert_eq!(restarted.get("build-7"), Some(settled));
    assert_eq!(restarted.list().len(), 1);
}

/// TC-PORT-JOB-3: work that was live when the process died is closed as
/// interrupted, and the closure is recorded.
///
/// The crash-repair discipline the session journal already has, restated for
/// jobs. Leaving a job `running` would make the store claim live work for
/// ever; deleting it would lose the fact that the work was cut off.
///
/// Expected: both live jobs become `Interrupted` with a reason; the settled
/// one is untouched; and a *second* reopen finds nothing to repair, because
/// the repair was appended rather than assumed.
#[test]
fn a_restart_closes_what_was_live_and_says_so() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("jobs.jsonl");

    {
        let store = JobStore::open(&path).expect("open");
        store
            .queue(Some("queued-1"), "bash", "never started", None)
            .unwrap();
        let running = store
            .queue(Some("running-1"), "bash", "in flight", None)
            .unwrap();
        store.start(&running.id).unwrap();
        let done = store
            .queue(Some("done-1"), "bash", "finished", None)
            .unwrap();
        store.start(&done.id).unwrap();
        store
            .finish(&done.id, JobStatus::Completed, None, Some("output"))
            .unwrap();
    }

    let after = JobStore::open(&path).expect("reopen");
    for id in ["queued-1", "running-1"] {
        let job = after.get(id).expect("still recorded");
        assert_eq!(job.status, JobStatus::Interrupted, "{id}");
        assert!(job.detail.is_some(), "{id} says why");
        assert!(job.finished_at.is_some(), "{id}");
    }
    let untouched = after.get("done-1").expect("recorded");
    assert_eq!(untouched.status, JobStatus::Completed);
    assert_eq!(untouched.output.as_deref(), Some("output"));
    assert!(after.live().is_empty());

    let lines_after_first = std::fs::read_to_string(&path).unwrap().lines().count();
    let again = JobStore::open(&path).expect("reopen twice");
    assert!(again.live().is_empty());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().lines().count(),
        lines_after_first,
        "the repair was appended, so a second open has nothing left to do"
    );
}

/// TC-PORT-JOB-4: a job ends once.
///
/// Upstream keeps the same rule through its `reported` flag. A second terminal
/// record would make "how did this end" a question with two answers.
///
/// Expected: the second `finish` is refused naming the status it already has,
/// and the record is unchanged.
#[test]
fn a_job_cannot_end_twice() {
    let (store, _dir) = store();
    let job = store.queue(Some("once"), "bash", "run", None).unwrap();
    store.start(&job.id).unwrap();
    store
        .finish(&job.id, JobStatus::Completed, Some("first"), None)
        .unwrap();

    let refused = store
        .finish(&job.id, JobStatus::Failed, Some("second"), None)
        .expect_err("already ended");
    assert!(
        matches!(
            refused,
            JobError::AlreadyEnded {
                status: JobStatus::Completed,
                ..
            }
        ),
        "got {refused:?}"
    );
    assert_eq!(store.get("once").unwrap().detail.as_deref(), Some("first"));
}

/// TC-PORT-JOB-5: a job that is not queued cannot start.
///
/// Expected: starting a running job and starting a settled one are both
/// refused, naming the status that refused them.
#[test]
fn only_a_queued_job_can_start() {
    let (store, _dir) = store();
    let job = store
        .queue(Some("start-once"), "bash", "run", None)
        .unwrap();
    store.start(&job.id).unwrap();

    assert!(matches!(
        store.start(&job.id).expect_err("already running"),
        JobError::NotStartable {
            status: JobStatus::Running,
            ..
        }
    ));

    store
        .finish(&job.id, JobStatus::Cancelled, None, None)
        .unwrap();
    assert!(matches!(
        store.start(&job.id).expect_err("already ended"),
        JobError::NotStartable {
            status: JobStatus::Cancelled,
            ..
        }
    ));
}

/// TC-PORT-JOB-6: an unknown job is an error, not a silent no-op.
///
/// Expected: every operation on an id the store does not hold reports it.
#[test]
fn an_unknown_job_is_reported() {
    let (store, _dir) = store();

    assert!(store.get("ghost").is_none());
    assert!(matches!(
        store.start("ghost").expect_err("no such job"),
        JobError::NoSuchJob(id) if id == "ghost"
    ));
    assert!(matches!(
        store
            .finish("ghost", JobStatus::Completed, None, None)
            .expect_err("no such job"),
        JobError::NoSuchJob(_)
    ));
}

/// TC-PORT-JOB-7: a caller-named id is refused when it is taken.
///
/// A caller that names its own id is saying the id means something outside the
/// store, so a duplicate is a collision to report rather than a job to reopen -
/// the same reasoning the session store applies to a fork's child id.
///
/// Expected: `Duplicate`, and the first job is untouched.
#[test]
fn a_named_id_that_is_taken_is_refused() {
    let (store, _dir) = store();
    store.queue(Some("taken"), "bash", "first", None).unwrap();

    let refused = store
        .queue(Some("taken"), "workflow", "second", None)
        .expect_err("taken");
    assert!(matches!(refused, JobError::Duplicate { .. }));
    assert_eq!(store.get("taken").unwrap().label, "first");
    assert_eq!(store.list().len(), 1);
}

/// TC-PORT-JOB-8: a minted id never collides with one the log already holds.
///
/// Expected: a store reopened over a log whose ids look minted still mints a
/// free one, rather than colliding with `bash-1`.
#[test]
fn a_minted_id_never_collides() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("jobs.jsonl");
    {
        let store = JobStore::open(&path).expect("open");
        store
            .queue(Some("bash-1"), "bash", "named to collide", None)
            .unwrap();
    }

    let store = JobStore::open(&path).expect("reopen");
    let minted = store.queue(None, "bash", "minted", None).expect("queue");
    assert_ne!(minted.id, "bash-1");
    assert_eq!(store.list().len(), 2);
    assert_eq!(store.get("bash-1").unwrap().label, "named to collide");
}

/// TC-PORT-JOB-9: jobs are listed oldest first, and by owner.
///
/// Upstream: `service.spec.ts`, "lists jobs for an owner". Access is fenced by
/// session id there; here the fence is the caller's, and this is the read it
/// fences on.
///
/// Expected: the whole list is in queue order, and the owner filter returns
/// only that session's jobs.
#[test]
fn jobs_are_listed_in_order_and_by_owner() {
    let (store, _dir) = store();
    store
        .queue(Some("a"), "bash", "first", Some("s-1"))
        .unwrap();
    store
        .queue(Some("b"), "bash", "second", Some("s-2"))
        .unwrap();
    store
        .queue(Some("c"), "workflow", "third", Some("s-1"))
        .unwrap();
    store.queue(Some("d"), "bash", "unowned", None).unwrap();

    let ids: Vec<String> = store.list().into_iter().map(|job| job.id).collect();
    assert_eq!(ids, vec!["a", "b", "c", "d"]);

    let mine: Vec<String> = store
        .owned_by("s-1")
        .into_iter()
        .map(|job| job.id)
        .collect();
    assert_eq!(mine, vec!["a", "c"]);
    assert!(store.owned_by("s-3").is_empty());
}

/// TC-PORT-JOB-10: a record a crash cut short is dropped, not refused.
///
/// The rule the session journal already follows: the newline is the commit, so
/// the only record a crash can cut is the last one, and it is a fact no caller
/// was ever told was durable.
///
/// Expected: the store opens, the whole records are read, the torn tail is
/// gone from the file, and the next append lands on a clean boundary.
#[test]
fn a_torn_tail_is_dropped_and_the_file_repaired() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("jobs.jsonl");
    {
        let store = JobStore::open(&path).expect("open");
        let job = store
            .queue(Some("whole"), "bash", "complete", None)
            .unwrap();
        store
            .finish(&job.id, JobStatus::Completed, None, None)
            .unwrap();
    }
    // A line the writer never finished.
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(r#"{"id":"torn","at":1,"op":"queue","kind":"bash""#);
    std::fs::write(&path, &text).unwrap();

    let store = JobStore::open(&path).expect("a torn tail is not a corrupt log");
    assert_eq!(store.get("whole").unwrap().status, JobStatus::Completed);
    assert!(store.get("torn").is_none());

    store.queue(Some("after"), "bash", "next", None).unwrap();
    let reread = JobStore::open(&path).expect("reopen");
    assert_eq!(reread.get("after").unwrap().label, "next");
    assert_eq!(reread.list().len(), 2);
}

/// TC-PORT-JOB-11: a committed line that does not parse is refused, by line.
///
/// The other half of the crash rule: a damaged line the writer *finished* is
/// not a crash tail, so the log is not the log that was written and is refused
/// rather than read past.
///
/// Expected: `Corrupt` naming line 2.
#[test]
fn a_damaged_committed_line_is_refused_by_line() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("jobs.jsonl");
    {
        let store = JobStore::open(&path).expect("open");
        store.queue(Some("first"), "bash", "one", None).unwrap();
    }
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("this is not a transition\n");
    std::fs::write(&path, &text).unwrap();

    let refused = JobStore::open(&path).expect_err("a finished line must parse");
    assert!(
        matches!(refused, JobError::Corrupt { line: 2, .. }),
        "got {refused:?}"
    );
}

/// TC-PORT-JOB-12: a name that is not safe everywhere is refused.
///
/// An id names a job in a log line, in a tool result and potentially in a
/// path, so one character set serves all three and a name is never escaped
/// differently depending on where it is shown.
///
/// Expected: each malformed shape is refused, and a legal one is not.
#[test]
fn an_unsafe_name_is_refused() {
    let (store, _dir) = store();
    for bad in ["", "has space", "has/slash", "has:colon"] {
        assert!(
            matches!(
                store.queue(Some(bad), "bash", "x", None),
                Err(JobError::BadName { .. })
            ),
            "{bad:?} should not be an id"
        );
        assert!(
            matches!(
                store.queue(None, bad, "x", None),
                Err(JobError::BadName { .. })
            ),
            "{bad:?} should not be a kind"
        );
    }
    assert!(store.queue(Some("fine.id-1_2"), "bash", "x", None).is_ok());
}
