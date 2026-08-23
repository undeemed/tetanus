//! Process execution: the seam everything that has to leave the harness runs
//! through.
//!
//! - [`proc`] runs one external command - argv without a shell re-split, an
//!   environment the caller listed, a working directory, captured stdio, an
//!   exit status or a signal, incremental output, and a termination that
//!   reaches the whole process group rather than one child.
//! - [`hooks`] runs a deployment's configured hooks through the same seam, so
//!   an out-of-process hook is a real child with a real process group.
//! - [`signals`] is the disposition every child here is started with, because
//!   an ignored signal is inherited across `exec` and a harness launched in
//!   the background would otherwise start shells that cannot be interrupted.
//! - [`piped`] is a child this process talks to rather than waits for: a
//!   protocol peer on stdio, ended over its own process group so what it
//!   started goes with it.
//! - [`backend`] is which shell that command goes through: `bash`, `pwsh`, and
//!   a loud refusal for a backend whose binary this host does not have.
//! - [`shell`] is one command through one backend: the deployment's defaults
//!   and caps, the run, and the text a model reads afterwards.
//! - [`session`] is a persistent shell: one long-lived process a turn reuses,
//!   keeping its directory and its variables between tool calls, with a death
//!   that is reported rather than restarted underneath the caller.
//! - [`pty`] is a real pseudo-terminal: the thing an interactive program
//!   needs and a pipe cannot give.
//! - [`terminal`] is a persistent terminal over one: a shell driven one send
//!   at a time, with a viewport, a bounded scrollback it can page back
//!   through, and a `^C` that reaches the command rather than the shell.
//! - [`terminals`] is which terminals a composition has open and who may
//!   touch them.
//! - [`tools`] and [`terminal_tools`] are what the model can actually call:
//!   `shell` for one command, `shell_open`/`shell_run`/`shell_close`/
//!   `shell_list` for the pipe-backed sessions, and the `terminal_*` family
//!   for the ones with a terminal underneath.
//!
//! Parity: upstream `packages/subprocess`, `packages/shell` and
//! `packages/terminal`, restated against this seam. `docs/parity.md` records
//! what is served and what is not.

pub mod backend;
pub mod hooks;
pub mod piped;
pub mod proc;
#[cfg(target_os = "linux")]
pub mod pty;
pub mod sanitize;
pub mod session;
pub mod shell;
pub mod signals;
#[cfg(target_os = "linux")]
pub mod terminal;
#[cfg(target_os = "linux")]
pub mod terminal_tools;
#[cfg(target_os = "linux")]
pub mod terminals;
pub mod tools;
pub mod transcript;

pub use backend::{BackendError, Bash, PowerShell, Resolved, ShellBackend};
pub use proc::{
    Captured, Chunk, Collected, Command, Ending, Limits, Output, OutputSink, ProcessError, Stream,
};
pub use session::{SessionConfig, SessionError, ShellSession, ShellSessions};
pub use shell::{ShellConfig, ShellError, ShellExec, ShellRequest, ShellRun, ShellSpec};
pub use tools::ShellTools;
