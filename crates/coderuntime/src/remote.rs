//! The remote backend: the same seam, somebody else's machine.
//!
//! Upstream's e2b package owns one sandbox and lends it to the capability
//! adapters that need a remote Linux world. This restates that ownership -
//! create it once, kill it on shutdown, never forward the key into it - and
//! puts a code runtime on top: submit, poll, fetch, cancel.
//!
//! **The provider is a trait, and the suite never leaves the machine.** A
//! backend that reached for a socket of its own would be a backend nobody
//! could test; [`Sandbox`] is the seam, [`double::ScriptedSandbox`] is what
//! the cases drive, and an HTTP implementation is a deployment's to add
//! against the same four calls.
//!
//! **Setting up is transactional.** A sandbox that was created and then could
//! not be prepared is killed before the failure is returned, because the
//! alternative is paying for a machine nobody is holding a handle to. If the
//! rollback also fails, the *original* failure is what the caller reads: the
//! second one is why the first could not be cleaned up, not what went wrong.
//!
//! **Killing something that is already gone is success.** Upstream says the
//! same in the case it spells "accepts a missing sandbox when disposal itself
//! requests deletion": teardown is idempotent, and only teardown - a
//! `NotFound` from a *submit* is a real failure.
//!
//! **A program's bindings do not cross the wire.** There is no bridge back
//! from a remote sandbox into this process's closures, so a request carrying
//! bindings is refused as seam misuse rather than silently running a program
//! whose `tools.read(...)` is undefined. Upstream has no such bridge either;
//! `docs/parity.md` records it as a gap rather than an accident.

pub mod double;

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::types::{check_bindings, CodeRuntime, FailureKind, RunRequest, RunResult, SeamError};

/// The provider's own identifier for one sandbox.
pub type SandboxId = String;

/// The provider's own identifier for one submitted program.
pub type JobId = String;

/// What a provider says about a job it is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    /// Still going.
    Running,
    /// Finished; the result is there to fetch.
    Done,
    /// Cancelled at this client's request.
    Cancelled,
    /// The sandbox died under it.
    Gone(String),
}

/// Why a call to the provider failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RemoteFault {
    /// No credential, or one the provider refused.
    #[error("{0}")]
    Unauthorized(String),
    /// The sandbox or the job is not there.
    #[error("{0}")]
    NotFound(String),
    /// Anything else the provider or the network did.
    #[error("{0}")]
    Provider(String),
}

/// One remote execution provider: create a sandbox, run programs in it, take
/// it down.
///
/// Four calls and two lifecycle ones, because that is what a provider of this
/// shape actually offers and anything richer would be this crate inventing an
/// API nobody serves.
#[async_trait::async_trait]
pub trait Sandbox: Send + Sync {
    /// The provider's name, for diagnostics.
    fn provider(&self) -> &str;

    /// Start a sandbox and answer its id.
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxId, RemoteFault>;

    /// Anything that has to be true before a program runs there - a working
    /// directory, a package, a file. Separate from `create` because it is the
    /// step whose failure has to roll the creation back.
    async fn prepare(&self, sandbox: &SandboxId, config: &SandboxConfig)
        -> Result<(), RemoteFault>;

    /// Hand over one program to run.
    async fn submit(&self, sandbox: &SandboxId, program: &str) -> Result<JobId, RemoteFault>;

    /// Ask how a job is doing.
    async fn poll(&self, job: &JobId) -> Result<JobState, RemoteFault>;

    /// Fetch what a finished job produced.
    async fn result(&self, job: &JobId) -> Result<RunResult, RemoteFault>;

    /// Ask for a job to stop.
    async fn cancel(&self, job: &JobId) -> Result<(), RemoteFault>;

    /// Take the sandbox down. A sandbox that is already gone is success.
    async fn kill(&self, sandbox: &SandboxId) -> Result<(), RemoteFault>;
}

/// How the remote runtime is set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxConfig {
    /// The credential. Never forwarded into the sandbox: a program running out
    /// there must not be able to create more sandboxes on this account.
    pub api_key: Option<String>,
    /// The working directory prepared before any program runs.
    pub cwd: String,
    /// How long the provider should keep the sandbox alive.
    pub lifetime: Duration,
    /// How long to wait between polls.
    pub poll_every: Duration,
    /// How long one program may take, start to finish.
    pub wall: Duration,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            // Upstream's default, so a deployment moving between the two finds
            // its files where it left them.
            cwd: "/home/user/workspace".to_string(),
            lifetime: Duration::from_secs(300),
            poll_every: Duration::from_millis(200),
            wall: Duration::from_secs(120),
        }
    }
}

impl SandboxConfig {
    /// The key, from the config or from the environment behind it.
    ///
    /// `from_env` is passed in rather than read here so a case can pin the
    /// fallback without setting a variable every other case in the binary
    /// shares.
    pub fn key(&self, from_env: Option<&str>) -> Result<String, RemoteFault> {
        self.api_key
            .as_deref()
            .or(from_env)
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                RemoteFault::Unauthorized(
                    "no API key for the remote code runtime: set one in the settings document or \
                     in the environment"
                        .to_string(),
                )
            })
    }
}

/// The sandbox this runtime is holding, if it has got one yet.
#[derive(Default)]
enum Holding {
    #[default]
    None,
    Live(SandboxId),
    /// Shut down. Nothing else will be created.
    Closed,
}

/// Runs programs on a remote provider, behind the same seam as the local one.
pub struct RemoteRuntime {
    provider: Arc<dyn Sandbox>,
    config: SandboxConfig,
    /// The shared sandbox. A mutex rather than a once-cell because setup can
    /// fail and has to be retried by whoever asks next.
    holding: Mutex<Holding>,
}

impl RemoteRuntime {
    pub fn new(provider: Arc<dyn Sandbox>, config: SandboxConfig) -> Self {
        Self {
            provider,
            config,
            holding: Mutex::new(Holding::None),
        }
    }

    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// The sandbox, creating and preparing it on first use.
    ///
    /// Setup is transactional: a sandbox that could not be prepared is killed
    /// before this returns, and the failure the caller reads is the one that
    /// happened first.
    async fn sandbox(&self) -> Result<SandboxId, RemoteFault> {
        let mut holding = self.holding.lock().await;
        match &*holding {
            Holding::Live(id) => return Ok(id.clone()),
            // Acquiring a handle after disposal has started is refused rather
            // than served by a sandbox this runtime is in the middle of
            // killing.
            Holding::Closed => {
                return Err(RemoteFault::Provider(
                    "this remote runtime has been shut down".to_string(),
                ))
            }
            Holding::None => {}
        }

        let id = self.provider.create(&self.config).await?;
        if let Err(setup) = self.provider.prepare(&id, &self.config).await {
            match self.provider.kill(&id).await {
                Ok(()) => {}
                // The rollback failed too. The setup failure is still what the
                // caller needs; this one is why the machine is still running,
                // and it belongs in the log rather than in place of it.
                Err(rollback) => tracing::error!(
                    provider = self.provider.provider(),
                    %rollback,
                    "a sandbox could not be killed after its setup failed"
                ),
            }
            return Err(setup);
        }
        *holding = Holding::Live(id.clone());
        Ok(id)
    }
}

#[async_trait::async_trait]
impl CodeRuntime for RemoteRuntime {
    fn language(&self) -> &str {
        crate::local::LANGUAGE
    }

    fn isolation(&self) -> &str {
        "container"
    }

    async fn run(&self, request: RunRequest) -> Result<RunResult, SeamError> {
        check_bindings(&request.bindings)?;
        if !request.bindings.is_empty() {
            // Said plainly rather than run and left to fail inside the
            // sandbox as an undefined name.
            return Err(SeamError::Unsupported {
                runtime: self.provider.provider().to_string(),
                why: "a remote sandbox cannot call this process's bindings; run a program that \
                      needs them on the local runtime"
                    .to_string(),
            });
        }
        if request.abort.is_stopped() {
            return Ok(RunResult::failed(
                FailureKind::Abort,
                "the run was aborted before it was submitted",
            ));
        }

        let started = Instant::now();
        let sandbox = match self.sandbox().await {
            Ok(sandbox) => sandbox,
            Err(fault) => return Ok(remote_failure(&fault, started)),
        };
        let job = match self.provider.submit(&sandbox, &request.program).await {
            Ok(job) => job,
            Err(fault) => return Ok(remote_failure(&fault, started)),
        };

        Ok(self.await_job(&job, &request, started).await)
    }

    async fn shutdown(&self) {
        let mut holding = self.holding.lock().await;
        let held = std::mem::replace(&mut *holding, Holding::Closed);
        if let Holding::Live(id) = held {
            match self.provider.kill(&id).await {
                // A sandbox that is already gone is the state this call wanted.
                Ok(()) | Err(RemoteFault::NotFound(_)) => {}
                Err(fault) => tracing::error!(
                    provider = self.provider.provider(),
                    %fault,
                    "a sandbox could not be killed at shutdown"
                ),
            }
        }
    }
}

impl RemoteRuntime {
    /// Poll until the job settles, the caller gives up, or the ceiling passes.
    async fn await_job(&self, job: &JobId, request: &RunRequest, started: Instant) -> RunResult {
        loop {
            if request.abort.is_stopped() {
                return self
                    .stop(job, started, FailureKind::Abort, "the run was aborted")
                    .await;
            }
            if started.elapsed() > self.config.wall {
                let why = format!(
                    "the remote run passed its ceiling of {}ms",
                    self.config.wall.as_millis()
                );
                return self.stop(job, started, FailureKind::Timeout, &why).await;
            }
            match self.provider.poll(job).await {
                Ok(JobState::Running) => tokio::time::sleep(self.config.poll_every).await,
                Ok(JobState::Done) => return self.fetch(job, started).await,
                Ok(JobState::Cancelled) => {
                    return RunResult {
                        duration: started.elapsed(),
                        ..RunResult::failed(FailureKind::Abort, "the remote run was cancelled")
                    }
                }
                // The sandbox died under the job: not the program's failure,
                // and not a timeout either.
                Ok(JobState::Gone(why)) => {
                    self.forget().await;
                    return RunResult {
                        duration: started.elapsed(),
                        ..RunResult::failed(FailureKind::WorkerExit, why)
                    };
                }
                Err(fault) => return remote_failure(&fault, started),
            }
        }
    }

    /// Fetch what a finished job produced.
    async fn fetch(&self, job: &JobId, started: Instant) -> RunResult {
        match self.provider.result(job).await {
            Ok(mut result) => {
                // The provider measures the program; this measures the round
                // trip, which is what the caller actually waited.
                result.duration = started.elapsed();
                result
            }
            Err(fault) => remote_failure(&fault, started),
        }
    }

    /// Cancel a job and report why this client stopped waiting for it.
    ///
    /// The cancel is best effort: the caller has already stopped waiting, and
    /// a provider that will not take the cancellation is a fact for the log,
    /// not a second failure to report over the first.
    async fn stop(&self, job: &JobId, started: Instant, kind: FailureKind, why: &str) -> RunResult {
        if let Err(fault) = self.provider.cancel(job).await {
            tracing::warn!(provider = self.provider.provider(), %fault, "a remote job would not cancel");
        }
        RunResult {
            duration: started.elapsed(),
            ..RunResult::failed(kind, why)
        }
    }

    /// Drop the sandbox this runtime was holding, so the next run makes a new
    /// one.
    async fn forget(&self) {
        let mut holding = self.holding.lock().await;
        if matches!(&*holding, Holding::Live(_)) {
            *holding = Holding::None;
        }
    }
}

/// A provider failure, as a run result.
///
/// Everything the provider can do to a run is `worker-exit` except a refused
/// credential, which is the deployment's to fix and is worth saying so.
fn remote_failure(fault: &RemoteFault, started: Instant) -> RunResult {
    RunResult {
        duration: started.elapsed(),
        ..RunResult::failed(FailureKind::WorkerExit, fault.to_string())
    }
}
