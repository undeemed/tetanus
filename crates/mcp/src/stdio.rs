//! An MCP server as a child process, spoken to over its standard input and
//! output.
//!
//! `tetanus_turn::process` runs a command to completion and hands back what it
//! printed. That is the wrong shape here: an MCP server is a long conversation
//! with a program that stays up, so this spawns and keeps the pipes rather
//! than collecting them.
//!
//! **The child's environment is what the caller listed.** The reasoning is
//! `crates/turn/src/process.rs`'s, and it applies harder here: a server named
//! in a settings document is a program a deployment chose, and handing it
//! every credential this process holds is a decision nobody made on purpose.
//!
//! **Stopping is a ladder with a floor.** Close the child's input, wait out
//! the grace period, kill. `kill_on_drop` is the backstop for the paths that
//! never reach the ladder - a panic, a dropped handle, a process exiting.
//! Between them, a server that ignores the polite half still goes away.
//!
//! **The child's stderr is not this crate's to interpret.** It is inherited,
//! so a server's own diagnostics land where the operator's do rather than
//! filling a buffer nobody reads. What must never be inherited is stdout: that
//! is the protocol.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

use crate::link::{Departure, Exit, Link, LinkReader, LinkWriter};

/// How long a child gets to exit on its own after its input is closed, before
/// it is killed.
///
/// Upstream's stdio transport spends two two-second grace periods and then
/// gives up; one bounded wait with a kill at the end reaches the same state
/// faster and has one fewer way to be left half-stopped.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(2);

/// A server this harness may start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCommand {
    pub program: String,
    pub args: Vec<String>,
    /// The child's whole environment. Empty means empty: see the module note.
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    /// How long the child gets to exit after its input is closed.
    pub grace: Duration,
}

impl ServerCommand {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            grace: DEFAULT_GRACE,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Start it, and hand back the two halves of the conversation.
    pub fn spawn(&self) -> io::Result<Link> {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&self.args)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherited on purpose: a server's log lines belong beside the
            // harness's, not in a buffer this crate would have to drain.
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }

        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let pid = child.id();
        Ok(Link::of_process(
            Box::new(ChildWriter {
                stdin: Some(stdin),
                child: Some(child),
                pid,
                grace: self.grace,
            }),
            Box::new(ChildReader {
                lines: BufReader::new(stdout).lines(),
            }),
            pid,
        ))
    }
}

struct ChildWriter {
    /// Taken by [`LinkWriter::stop`]: dropping it is what closes the pipe.
    stdin: Option<ChildStdin>,
    /// Taken by the first stop, so a second one has nothing left to do.
    child: Option<Child>,
    pid: Option<u32>,
    grace: Duration,
}

#[async_trait::async_trait]
impl LinkWriter for ChildWriter {
    async fn send(&mut self, line: &str) -> io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "the server's input is closed")
        })?;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await
    }

    async fn stop(&mut self) -> Departure {
        // Closing the input is the whole polite half: an MCP server on stdio
        // has no shutdown request, it exits when its input ends.
        drop(self.stdin.take());
        let Some(mut child) = self.child.take() else {
            return Departure {
                exit: Exit::Closed,
                pid: self.pid,
            };
        };
        let exit = match tokio::time::timeout(self.grace, child.wait()).await {
            Ok(Ok(status)) => status.code().map_or(Exit::Unknown, Exit::Code),
            // Waiting failed, which leaves no way to learn the status. The
            // kill below still runs: an unknown state is not a reason to walk
            // away from a running child.
            Ok(Err(_)) => Exit::Unknown,
            Err(_) => {
                let _ = child.kill().await;
                // `kill` awaits the child, so by here it is reaped rather than
                // a zombie. A test that looks for the pid finds nothing.
                Exit::Killed
            }
        };
        Departure {
            exit,
            pid: self.pid,
        }
    }
}

struct ChildReader {
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
}

#[async_trait::async_trait]
impl LinkReader for ChildReader {
    async fn recv(&mut self) -> Option<io::Result<String>> {
        self.lines.next_line().await.transpose()
    }
}
