//! Process execution: the seam everything that has to leave the harness runs
//! through.
//!
//! - [`proc`] runs one external command - argv without a shell re-split, an
//!   environment the caller listed, a working directory, captured stdio, an
//!   exit status or a signal, incremental output, and a termination that
//!   reaches the whole process group rather than one child.
//! - [`backend`] is which shell that command goes through: `bash`, `pwsh`, and
//!   a loud refusal for a backend whose binary this host does not have.
//! - [`shell`] is one command through one backend: the deployment's defaults
//!   and caps, the run, and the text a model reads afterwards.
//!
//! Parity: upstream `packages/subprocess`, `packages/shell` and
//! `packages/terminal`, restated against this seam. `docs/parity.md` records
//! what is served and what is not.

pub mod backend;
pub mod proc;
pub mod shell;

pub use backend::{BackendError, Bash, PowerShell, Resolved, ShellBackend};
pub use proc::{
    Captured, Chunk, Collected, Command, Ending, Limits, Output, OutputSink, ProcessError, Stream,
};
pub use shell::{ShellConfig, ShellError, ShellExec, ShellRequest, ShellRun, ShellSpec};
