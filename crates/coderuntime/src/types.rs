//! What a caller hands a runtime, and what it gets back.
//!
//! Vocabulary only: nothing here evaluates anything. The seam is deliberately
//! this small, because a backend that runs a program in this process and one
//! that runs it in somebody else's datacentre have to be the same trait or the
//! caller ends up choosing between them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

/// One host function the program may call.
///
/// It takes one JSON value and answers one, or a message the program sees as
/// its own failure. Synchronous, and called on the worker: upstream's bindings
/// are async because a Node worker can await across a port, and this crate has
/// no such bridge yet - `docs/parity.md` carries the difference rather than a
/// signature that pretends otherwise.
pub type Binding = Arc<dyn Fn(&Value) -> Result<Value, String> + Send + Sync>;

/// A named group of bindings, exposed to the program as one global object.
#[derive(Clone, Default)]
pub struct Namespace {
    /// The global the program calls it by. Checked against
    /// [`crate::reserved`]: portable identifiers only, no reserved word of any
    /// target language, and no slot a backend owns.
    pub global: String,
    /// The callable members, by the exact name the program writes.
    pub functions: std::collections::BTreeMap<String, Binding>,
}

impl std::fmt::Debug for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Namespace")
            .field("global", &self.global)
            .field("functions", &self.functions.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Namespace {
    pub fn new(global: impl Into<String>) -> Self {
        Self {
            global: global.into(),
            functions: std::collections::BTreeMap::new(),
        }
    }

    pub fn with(
        mut self,
        name: impl Into<String>,
        body: impl Fn(&Value) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        self.functions.insert(name.into(), Arc::new(body));
        self
    }
}

/// Ask the run to stop. Shared with the caller, read by the evaluator on every
/// step: the only way to stop a program that will not stop itself.
///
/// An abort can watch more flags than its own ([`Abort::and`]), because a run
/// is stopped by two independent things - the caller cancelling it and the
/// runtime shutting down - and the evaluator should read one flag per step
/// rather than have a thread somewhere OR them together for it.
#[derive(Debug, Clone, Default)]
pub struct Abort {
    own: Arc<AtomicBool>,
    /// Flags belonging to somebody else that also stop this run.
    also: Vec<Arc<AtomicBool>>,
}

impl Abort {
    pub fn new() -> Self {
        Self::default()
    }

    /// An abort that has already fired, for a caller that was cancelled before
    /// it got as far as running anything.
    pub fn fired() -> Self {
        let abort = Self::new();
        abort.stop();
        abort
    }

    /// Stop the runs watching this abort. Never stops the flags it merely
    /// watches: a run cancelling itself must not shut a runtime down.
    pub fn stop(&self) {
        self.own.store(true, Ordering::Release);
    }

    pub fn is_stopped(&self) -> bool {
        self.own.load(Ordering::Acquire)
            || self.also.iter().any(|flag| flag.load(Ordering::Acquire))
    }

    /// An abort that fires when this one fires, and when `other` does.
    pub fn and(&self, other: &Abort) -> Abort {
        let mut also = self.also.clone();
        also.push(Arc::clone(&other.own));
        also.extend(other.also.iter().map(Arc::clone));
        Abort {
            own: Arc::clone(&self.own),
            also,
        }
    }
}

/// One run: the program, what it may call, and how to stop it.
#[derive(Debug, Clone, Default)]
pub struct RunRequest {
    pub program: String,
    pub bindings: Vec<Namespace>,
    /// Fires to stop the run. A request that arrives already stopped never
    /// starts a worker.
    pub abort: Abort,
}

impl RunRequest {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            bindings: Vec::new(),
            abort: Abort::new(),
        }
    }

    pub fn binding(mut self, namespace: Namespace) -> Self {
        self.bindings.push(namespace);
        self
    }

    pub fn abort_with(mut self, abort: Abort) -> Self {
        self.abort = abort;
        self
    }
}

/// Why a run failed.
///
/// Upstream's taxonomy, kind for kind, because these are orthogonal outcomes
/// and collapsing any two of them loses the one fact the reader needs: a
/// budget expiring is not an exception, an abort is not a timeout, and a
/// substrate that died is neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    /// The program threw, or would not parse.
    Exception,
    /// A budget this runtime owns ran out. The message says which.
    Timeout,
    /// [`RunRequest::abort`] fired.
    Abort,
    /// The substrate died without answering: a panicked worker, a sandbox that
    /// went away.
    WorkerExit,
    /// The completion value is not lossless JSON.
    InvalidOutput,
    /// The logs and the value together exceeded the configured cap.
    OutputLimit,
}

impl FailureKind {
    /// The one word a failed result leads with, and what a journal is searched
    /// by.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exception => "exception",
            Self::Timeout => "timeout",
            Self::Abort => "abort",
            Self::WorkerExit => "worker-exit",
            Self::InvalidOutput => "invalid-output",
            Self::OutputLimit => "output-limit",
        }
    }
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failure, with enough detail for a model to correct itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunFailure {
    pub kind: FailureKind,
    pub message: String,
}

impl RunFailure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// What one run produced.
///
/// A failure is a field here, never an error of [`CodeRuntime::run`]: a
/// program that threw is news for the caller to report, not an exception path
/// for it to handle.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunResult {
    /// The program's completion value, when it ran to one and the value
    /// crossed the lossless-JSON boundary.
    pub value: Option<Value>,
    /// What the program logged, in order.
    pub logs: Vec<String>,
    /// Present exactly when the run failed.
    pub error: Option<RunFailure>,
    /// How long the run took, measured by the runtime that ran it.
    pub duration: Duration,
}

impl RunResult {
    pub fn ok(value: Option<Value>, logs: Vec<String>, duration: Duration) -> Self {
        Self {
            value,
            logs,
            error: None,
            duration,
        }
    }

    pub fn failed(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            value: None,
            logs: Vec::new(),
            error: Some(RunFailure::new(kind, message)),
            duration: Duration::ZERO,
        }
    }

    /// Whether the program ran to completion.
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }

    /// The failure's class, for a caller rendering a result.
    pub fn kind(&self) -> Option<FailureKind> {
        self.error.as_ref().map(|failure| failure.kind)
    }
}

/// Misuse of the seam itself, refused before anything runs.
///
/// The line is upstream's: a program that fails is a [`RunResult`], and only a
/// caller that asked for something incoherent - two namespaces of one name, a
/// global no backend can expose - is an error. A caller cannot fix the first
/// by changing its code; it can always fix the second.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SeamError {
    #[error("binding namespace {global:?} cannot be exposed: {why}")]
    BadNamespace { global: String, why: String },
    #[error("two binding namespaces are both called {0:?}")]
    DuplicateNamespace(String),
    #[error("{runtime} cannot run this request: {why}")]
    Unsupported { runtime: String, why: String },
}

/// The seam: evaluate a program, answer with what it produced.
#[async_trait::async_trait]
pub trait CodeRuntime: Send + Sync {
    /// The language `program` is written in, as a lowercase identifier.
    /// Informational: a surface that presents usage instructions switches on
    /// it, and nothing gates on it.
    fn language(&self) -> &str;

    /// The substrate, as a lowercase identifier - `worker-thread`, `process`,
    /// `container`. A descriptor for diagnostics, not a security claim.
    fn isolation(&self) -> &str;

    /// Run one program. Only seam misuse is an `Err`.
    async fn run(&self, request: RunRequest) -> Result<RunResult, SeamError>;

    /// Stop accepting runs, end the ones in flight, and release whatever the
    /// backend is holding. Runs after this are refused.
    async fn shutdown(&self);
}

/// Check a request's namespaces against the portable rules, refusing misuse
/// before a backend spends anything on it.
///
/// Every backend calls this, which is what makes the promise real: a request
/// that is valid for the local runtime is valid for the remote one.
pub fn check_bindings(bindings: &[Namespace]) -> Result<(), SeamError> {
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for namespace in bindings {
        if let Err(why) = crate::reserved::check_global(&namespace.global) {
            return Err(SeamError::BadNamespace {
                global: namespace.global.clone(),
                why,
            });
        }
        if !seen.insert(namespace.global.as_str()) {
            return Err(SeamError::DuplicateNamespace(namespace.global.clone()));
        }
    }
    Ok(())
}
