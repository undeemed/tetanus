//! The kernel boundary, applied to the filesystem service.
//!
//! [`crate::sandbox::SandboxedFs`] decides which paths this process's own
//! syscalls may name, and says at length that it is containment rather than a
//! security boundary. This is the other half: the same [`Policy`] the shell
//! tools run behind, enforced by the kernel on the operations the file tools
//! perform. A path both layers allow is written; a path the fence allows and
//! the policy does not is refused by Landlock, with no cooperation from this
//! code at all.
//!
//! **Why a thread.** Landlock restricts *a thread* and everything it later
//! starts, and it cannot be undone. The harness has to keep writing its
//! journal, reading its settings and answering its socket, so restricting the
//! process's own threads is not available: what is confined is one worker
//! thread that restricts itself the moment it starts, and every file operation
//! is performed there. That worker is the only place in the process where the
//! policy applies, which is exactly the property that lets the harness stay
//! usable while the model's file tools do not.
//!
//! **Operations are serial.** One worker means one file operation at a time.
//! That is a real cost and it is chosen deliberately: a pool would need one
//! restriction per thread and a policy change would then race threads that
//! have already restricted themselves. The tool pipeline already treats a
//! mutation as a barrier, so the cost lands mostly on parallel reads, and
//! `docs/parity.md` records widening it as its own slice.
//!
//! **A denial arrives already classified.** The vocabulary in
//! [`crate::error`] separates `SandboxDenied` - this build decided - from
//! `PermissionDenied` - the operating system refused. A Landlock denial is
//! `EACCES` from an ordinary syscall, so it lands in the second class without
//! this module translating anything, and a model reading the result can tell
//! which of the two happened.
//!
//! Parity: upstream confines its bash runner and fences its filesystem
//! separately; this applies one policy to both, which is the arrangement its
//! `sandbox/src/roots.ts` note asks for ("the write tool cannot write /tmp but
//! bash can" is the asymmetry a shared derivation prevents).

use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use tetanus_sandbox::{Enforcement, Policy, SandboxError};

use crate::access::FsMode;
use crate::error::FsError;
use crate::service::{
    Deleted, DirEntry, EditOutcome, EditRequest, FileSystem, FsInfo, FsTarget, FsVersion,
    WriteIntent, WriteOutcome,
};

/// One unit of work for the confined thread.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// A filesystem whose operations happen behind a kernel boundary.
///
/// Wraps any [`FileSystem`]: the fence and the mode of the inner backend still
/// apply and are still checked first, because a refusal this build can explain
/// in a sentence is worth more to a model than an `EACCES` it has to guess
/// about. The kernel is what makes the refusal true when the explanation is
/// wrong.
pub struct KernelConfined {
    inner: Arc<dyn FileSystem>,
    jobs: Mutex<Sender<Job>>,
    enforcement: Enforcement,
}

impl KernelConfined {
    /// Confine `inner` under `policy`.
    ///
    /// The worker restricts itself before this returns, so a host that cannot
    /// enforce the policy fails here - at composition, where a deployment can
    /// be told - and never by handing back a service that quietly enforces
    /// nothing.
    pub fn new(inner: Arc<dyn FileSystem>, policy: Policy) -> Result<Self, SandboxError> {
        let (send_job, take_job) = channel::<Job>();
        let (send_ready, ready) = channel::<Result<Enforcement, SandboxError>>();

        std::thread::Builder::new()
            .name("tetanus-fs-confined".to_string())
            .spawn(move || {
                // The first thing this thread does, before it will accept any
                // work at all: restrict itself, one way, for the rest of its
                // life.
                let restricted = tetanus_sandbox::landlock::confine_current_thread(&policy);
                let ok = restricted.is_ok();
                // A caller waiting on this is either about to use the worker or
                // about to give up on it; either way it needs the answer.
                if send_ready.send(restricted).is_err() || !ok {
                    return;
                }
                // Every file operation of this service happens here, inside the
                // boundary.
                while let Ok(job) = take_job.recv() {
                    job();
                }
            })
            .map_err(|source| SandboxError::Kernel {
                backend: "landlock",
                what: "start the confined filesystem worker",
                source,
            })?;

        let enforcement = ready.recv().map_err(|_| SandboxError::Kernel {
            backend: "landlock",
            what: "hear back from the confined filesystem worker",
            source: std::io::Error::other("the worker stopped before it reported"),
        })??;

        Ok(Self {
            inner,
            jobs: Mutex::new(send_job),
            enforcement,
        })
    }

    /// How completely this host enforces the policy behind the service.
    pub fn enforcement(&self) -> Enforcement {
        self.enforcement
    }

    /// Run one operation inside the boundary and wait for its answer.
    ///
    /// A worker that has died takes the service with it: the answer is an
    /// error naming that, never a silent fall through to an unconfined call in
    /// this thread. Falling through is the one failure this whole module
    /// exists to prevent.
    fn inside<T, F>(&self, operation: &'static str, path: String, body: F) -> Result<T, FsError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn FileSystem) -> Result<T, FsError> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        let (send, receive) = channel();
        let job: Job = Box::new(move || {
            let answer = body(inner.as_ref());
            // The receiver is gone only if the caller was dropped mid-call,
            // which is not this side's problem to report.
            let _ = send.send(answer);
        });

        let dispatched = self
            .jobs
            .lock()
            .expect("no panic holds this lock")
            .send(job);
        if dispatched.is_err() {
            return Err(gone(operation, path));
        }
        receive
            .recv()
            .unwrap_or_else(|_| Err(gone(operation, path)))
    }
}

/// The worker is not there any more, so nothing can be done inside the
/// boundary - and nothing will be done outside it.
fn gone(operation: &'static str, path: String) -> FsError {
    FsError::Io {
        path,
        operation,
        message: "the confined filesystem worker is gone, so this operation cannot be performed \
                  inside the sandbox; it was not attempted outside it"
            .to_string(),
    }
}

impl FileSystem for KernelConfined {
    fn backend(&self) -> &'static str {
        // Named for both halves, because a diagnostic that said only
        // "sandboxed" would hide which of the two refused.
        "kernel-confined"
    }

    fn mode(&self) -> FsMode {
        self.inner.mode()
    }

    fn resolve(&self, path: &str) -> Result<FsTarget, FsError> {
        // Resolution reads the filesystem to canonicalize, and reads are
        // permitted under every policy this crate can be given, but it still
        // runs inside: a resolution that walked a path the policy hides would
        // answer from outside the boundary.
        let requested = path.to_string();
        let display = path.to_string();
        self.inside("resolve", display, move |fs| fs.resolve(&requested))
    }

    fn stat(&self, target: &FsTarget) -> Result<Option<FsInfo>, FsError> {
        let target = target.clone();
        let path = target.display().to_string();
        self.inside("stat", path, move |fs| fs.stat(&target))
    }

    fn read(&self, target: &FsTarget) -> Result<(String, FsVersion), FsError> {
        let target = target.clone();
        let path = target.display().to_string();
        self.inside("read", path, move |fs| fs.read(&target))
    }

    fn read_bytes(
        &self,
        target: &FsTarget,
        offset: u64,
        len: u64,
    ) -> Result<(Vec<u8>, FsVersion), FsError> {
        let target = target.clone();
        let path = target.display().to_string();
        self.inside("read", path, move |fs| fs.read_bytes(&target, offset, len))
    }

    fn write(
        &self,
        target: &FsTarget,
        content: &str,
        intent: &WriteIntent,
    ) -> Result<WriteOutcome, FsError> {
        let target = target.clone();
        let content = content.to_string();
        let intent = intent.clone();
        let path = target.display().to_string();
        self.inside("write", path, move |fs| {
            fs.write(&target, &content, &intent)
        })
    }

    fn edit(
        &self,
        target: &FsTarget,
        edit: &EditRequest,
        guard: Option<&FsVersion>,
    ) -> Result<EditOutcome, FsError> {
        let target = target.clone();
        let edit = edit.clone();
        let guard = guard.cloned();
        let path = target.display().to_string();
        self.inside("edit", path, move |fs| {
            fs.edit(&target, &edit, guard.as_ref())
        })
    }

    fn list(&self, target: &FsTarget) -> Result<Vec<DirEntry>, FsError> {
        let target = target.clone();
        let path = target.display().to_string();
        self.inside("list", path, move |fs| fs.list(&target))
    }

    fn glob(&self, base: &FsTarget, pattern: &str) -> Result<Vec<FsTarget>, FsError> {
        let base = base.clone();
        let pattern = pattern.to_string();
        let path = base.display().to_string();
        self.inside("glob", path, move |fs| fs.glob(&base, &pattern))
    }

    fn delete(&self, target: &FsTarget, recursive: bool) -> Result<Deleted, FsError> {
        let target = target.clone();
        let path = target.display().to_string();
        self.inside("delete", path, move |fs| fs.delete(&target, recursive))
    }
}

/// The backend a mode asks for, with the kernel boundary a policy asks for in
/// front of it.
///
/// One call so a deployment never composes the pair by hand - the same reason
/// [`crate::access::backend`] exists, extended by one layer. A policy that
/// confines nothing (`danger-full-access`, written out) composes the plain
/// backend, so the worker thread and its serialization are not paid for by a
/// deployment that asked for no boundary.
///
/// The fence and the policy are separate arguments on purpose. They are
/// usually the same root and the same intent, but not always: a deployment may
/// fence the model to one project directory while the kernel policy also
/// grants a shared cache the build needs. Collapsing them into one value would
/// make that arrangement unexpressible, and the day it is needed is the day
/// somebody turns the boundary off instead.
pub fn confined_backend(
    mode: FsMode,
    root: impl AsRef<std::path::Path>,
    policy: &Policy,
) -> Result<Arc<dyn FileSystem>, ConfinedBackendError> {
    let inner = crate::access::backend(mode, root)?;
    if !policy.mode().confines() {
        return Ok(inner);
    }
    Ok(Arc::new(KernelConfined::new(inner, policy.clone())?))
}

/// Why a confined backend could not be composed: the filesystem half, or the
/// kernel half.
///
/// Two classes rather than one string, because the answers differ: a bad root
/// is a configuration mistake a person fixes in a document, and an
/// unenforceable policy is a host that cannot run this deployment at all.
#[derive(Debug, thiserror::Error)]
pub enum ConfinedBackendError {
    #[error(transparent)]
    FileSystem(#[from] FsError),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
}
