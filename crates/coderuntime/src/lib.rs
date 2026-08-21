//! Running a program the model wrote, and getting back what it produced.
//!
//! A tool call is a function the harness chose and the model filled in. A
//! *program* is the other shape: the model writes the control flow - a loop, a
//! condition, three calls whose results feed each other - and the harness runs
//! it once. Upstream calls the seam `CodeRuntime` and this crate restates it:
//! [`CodeRuntime`] is the trait, [`RunRequest`] is one run, and [`RunResult`]
//! is what came back - a value, the logs in order, a failure with a class, and
//! how long it took.
//!
//! **A failed program is a result, never an error of `run`.** The taxonomy is
//! upstream's ([`FailureKind`]): a budget expiring is not an exception, an
//! abort is not a timeout, and a substrate that died is neither. Only misuse
//! of the seam itself - a binding namespace named `console`, say - is refused
//! before a run starts.
//!
//! **The language is not JavaScript, and this is the one place that matters.**
//! Upstream evaluates TypeScript in a Node worker thread. A Rust harness has
//! no JavaScript engine and will not grow one for this, so the local backend
//! evaluates [a small deterministic language](local) of its own. Parity is at
//! the *seam* - the request, the failure taxonomy, the caps, the binding rules
//! - and `docs/parity.md` says so rather than implying a JS runtime exists.
//!
//! **A runaway program is stopped because the evaluator agrees to be
//! stopped.** Node terminates a worker thread; Rust cannot terminate an OS
//! thread at all, and nothing short of a process boundary can. So the local
//! evaluator spends [fuel](local::Budget) and reads a cancellation flag on
//! every step: a budget that runs out, a deadline that passes, or an abort
//! ends the run at the next step and the worker thread is reclaimed rather
//! than leaked. That is why the language is small and interpreted here instead
//! of handed to something bigger.
//!
//! **This runtime executes no native code and touches nothing.** No
//! filesystem, no network, no subprocess: the program can only compute and
//! call the host bindings the caller passed in. There is therefore nothing for
//! a path fence or a sandbox mode to fence, and the parity note says which
//! enforcement a *future* backend - one that shells out to a real interpreter -
//! would need from the shell lane's `crates/exec` instead of guessing at it
//! here.

pub mod local;
pub mod remote;
pub mod reserved;
pub mod settings;
pub mod tool;
pub mod types;

pub use local::{Budget, LocalRuntime};
pub use remote::{RemoteRuntime, Sandbox, SandboxConfig};
pub use tool::CodeTool;
pub use types::{
    Binding, CodeRuntime, FailureKind, Namespace, RunFailure, RunRequest, RunResult, SeamError,
};
