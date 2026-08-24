//! A provider that answers the way a real one does, without leaving the
//! machine.
//!
//! It ships rather than living in the test tree, for the reason
//! `crates/web/src/mock.rs` gives: an offline demonstration, an air-gapped
//! evaluation and a reproduction of a bug all need a provider that behaves the
//! same way twice, and the suite is the first caller rather than the only
//! intended one.
//!
//! It really runs the program - on the local runtime, in a thread - so a case
//! that drives the remote backend end to end is asserting a real evaluation
//! that arrived through submit, poll and fetch, rather than a canned result.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::local::{Budget, LocalRuntime};
use crate::types::{CodeRuntime, RunRequest, RunResult};

use super::{JobId, JobState, RemoteFault, Sandbox, SandboxConfig, SandboxId};

/// What the double should do instead of behaving.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Faults {
    /// Refuse to create a sandbox.
    pub create: Option<RemoteFault>,
    /// Create one, then refuse to prepare it - the rollback case.
    pub prepare: Option<RemoteFault>,
    /// Refuse to kill it, so a rollback fails too.
    pub kill: Option<RemoteFault>,
    /// Refuse to submit.
    pub submit: Option<RemoteFault>,
    /// Answer this many polls with `Running` before the job settles, so a case
    /// can watch the poll loop rather than a single answer.
    pub polls_before_done: usize,
}

/// One job the double is holding.
struct Job {
    program: String,
    polled: usize,
    cancelled: bool,
}

/// A provider with no network behind it.
pub struct ScriptedSandbox {
    faults: Faults,
    created: AtomicUsize,
    killed: AtomicUsize,
    /// Sandboxes this double believes are alive.
    live: Mutex<Vec<SandboxId>>,
    jobs: Mutex<BTreeMap<JobId, Job>>,
    next: AtomicUsize,
    /// What each sandbox was prepared with, so a case can assert the config
    /// actually travelled.
    prepared: Mutex<Vec<String>>,
}

impl Default for ScriptedSandbox {
    fn default() -> Self {
        Self::new(Faults::default())
    }
}

impl ScriptedSandbox {
    pub fn new(faults: Faults) -> Self {
        Self {
            faults,
            created: AtomicUsize::new(0),
            killed: AtomicUsize::new(0),
            live: Mutex::new(Vec::new()),
            jobs: Mutex::new(BTreeMap::new()),
            next: AtomicUsize::new(1),
            prepared: Mutex::new(Vec::new()),
        }
    }

    /// How many sandboxes were ever created.
    pub fn created(&self) -> usize {
        self.created.load(Ordering::Acquire)
    }

    /// How many kills this provider was asked for.
    pub fn killed(&self) -> usize {
        self.killed.load(Ordering::Acquire)
    }

    /// The sandboxes it still believes are running. Empty is what a caller
    /// that shut down cleanly leaves behind.
    pub fn live(&self) -> Vec<SandboxId> {
        self.live.lock().expect("live").clone()
    }

    /// The working directories it was asked to prepare.
    pub fn prepared(&self) -> Vec<String> {
        self.prepared.lock().expect("prepared").clone()
    }

    /// Whether a job was cancelled.
    pub fn was_cancelled(&self, job: &str) -> bool {
        self.jobs
            .lock()
            .expect("jobs")
            .get(job)
            .is_some_and(|held| held.cancelled)
    }

    /// The ids of every job it was given.
    pub fn jobs(&self) -> Vec<JobId> {
        self.jobs.lock().expect("jobs").keys().cloned().collect()
    }

    fn mint(&self, what: &str) -> String {
        format!("{what}-{}", self.next.fetch_add(1, Ordering::AcqRel))
    }
}

#[async_trait::async_trait]
impl Sandbox for ScriptedSandbox {
    fn provider(&self) -> &str {
        "scripted"
    }

    async fn create(&self, config: &SandboxConfig) -> Result<SandboxId, RemoteFault> {
        // A provider asks for the key on every call that costs money; this
        // one asks once, which is enough to pin that the runtime never sends
        // it into the sandbox itself.
        config.key(None)?;
        if let Some(fault) = &self.faults.create {
            return Err(fault.clone());
        }
        self.created.fetch_add(1, Ordering::AcqRel);
        let id = self.mint("sandbox");
        self.live.lock().expect("live").push(id.clone());
        Ok(id)
    }

    async fn prepare(
        &self,
        sandbox: &SandboxId,
        config: &SandboxConfig,
    ) -> Result<(), RemoteFault> {
        if let Some(fault) = &self.faults.prepare {
            return Err(fault.clone());
        }
        if !self.live.lock().expect("live").contains(sandbox) {
            return Err(RemoteFault::NotFound(format!("no sandbox {sandbox}")));
        }
        self.prepared
            .lock()
            .expect("prepared")
            .push(config.cwd.clone());
        Ok(())
    }

    async fn submit(&self, sandbox: &SandboxId, program: &str) -> Result<JobId, RemoteFault> {
        if let Some(fault) = &self.faults.submit {
            return Err(fault.clone());
        }
        if !self.live.lock().expect("live").contains(sandbox) {
            return Err(RemoteFault::NotFound(format!("no sandbox {sandbox}")));
        }
        let id = self.mint("job");
        self.jobs.lock().expect("jobs").insert(
            id.clone(),
            Job {
                program: program.to_string(),
                polled: 0,
                cancelled: false,
            },
        );
        Ok(id)
    }

    async fn poll(&self, job: &JobId) -> Result<JobState, RemoteFault> {
        let mut jobs = self.jobs.lock().expect("jobs");
        let held = jobs
            .get_mut(job)
            .ok_or_else(|| RemoteFault::NotFound(format!("no job {job}")))?;
        if held.cancelled {
            return Ok(JobState::Cancelled);
        }
        held.polled += 1;
        if held.polled > self.faults.polls_before_done {
            Ok(JobState::Done)
        } else {
            Ok(JobState::Running)
        }
    }

    async fn result(&self, job: &JobId) -> Result<RunResult, RemoteFault> {
        let program = {
            let jobs = self.jobs.lock().expect("jobs");
            jobs.get(job)
                .ok_or_else(|| RemoteFault::NotFound(format!("no job {job}")))?
                .program
                .clone()
        };
        // Really evaluated, so an end-to-end case through submit/poll/fetch is
        // asserting a real run rather than a canned answer.
        let runtime = LocalRuntime::new(Budget {
            wall: Duration::from_secs(2),
            reap_grace: Duration::from_millis(200),
            ..Budget::default()
        });
        runtime
            .run(RunRequest::new(program))
            .await
            .map_err(|misuse| RemoteFault::Provider(misuse.to_string()))
    }

    async fn cancel(&self, job: &JobId) -> Result<(), RemoteFault> {
        let mut jobs = self.jobs.lock().expect("jobs");
        let held = jobs
            .get_mut(job)
            .ok_or_else(|| RemoteFault::NotFound(format!("no job {job}")))?;
        held.cancelled = true;
        Ok(())
    }

    async fn kill(&self, sandbox: &SandboxId) -> Result<(), RemoteFault> {
        self.killed.fetch_add(1, Ordering::AcqRel);
        if let Some(fault) = &self.faults.kill {
            return Err(fault.clone());
        }
        let mut live = self.live.lock().expect("live");
        match live.iter().position(|held| held == sandbox) {
            Some(at) => {
                live.remove(at);
                Ok(())
            }
            // A provider answers this way for a sandbox that has already
            // expired, which is the case the runtime must not treat as a
            // failure.
            None => Err(RemoteFault::NotFound(format!(
                "sandbox {sandbox} is already gone"
            ))),
        }
    }
}

/// A scripted provider and a runtime over it, wired the way a deployment would.
pub fn scripted(
    faults: Faults,
    config: SandboxConfig,
) -> (Arc<ScriptedSandbox>, super::RemoteRuntime) {
    let provider = Arc::new(ScriptedSandbox::new(faults));
    let runtime = super::RemoteRuntime::new(Arc::clone(&provider) as Arc<dyn Sandbox>, config);
    (provider, runtime)
}
