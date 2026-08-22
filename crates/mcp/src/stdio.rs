//! An MCP server as a child process, spoken to over its standard input and
//! output.
//!
//! The process itself is `tetanus_exec::piped`, which is the workspace's one
//! seam for a child this harness talks to rather than waits for. What stays
//! here is the part that is MCP's: line framing, and the vocabulary the
//! connection driver reads.
//!
//! It used to spawn its own child, and the difference is not cosmetic. That
//! version killed *the server*; a server that starts helpers of its own left
//! them behind holding their pipes. The shared seam gives every child a
//! process group of its own and ends the group, so a server's children go with
//! it - the same guarantee `crates/exec` has always made for a command, now
//! made for a peer.
//!
//! **The child's environment is what the caller listed**, and **stderr is
//! inherited while stdout is piped**: both are the seam's rules, and the
//! second is the one that matters most here, because a server whose protocol
//! stream was inherited would print its frames onto the terminal and answer
//! nobody.
//!
//! **Stopping is polite first.** An MCP server on stdio has no shutdown
//! request - it exits when its input ends - so the ladder starts by closing
//! stdin and only reaches for signals when the grace period is spent.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use tetanus_exec::piped::{PipedChild, PipedCommand, PipedExit};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

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
        let mut command = PipedCommand::new(&self.program)
            .args(self.args.clone())
            .envs(self.env.clone())
            .grace(self.grace);
        if let Some(dir) = &self.cwd {
            command = command.cwd(dir);
        }

        let mut child = command.spawn()?;
        let stdin = child.stdin().expect("stdin was piped");
        let stdout = child.stdout().expect("stdout was piped");
        let pid = child.pid();
        Ok(Link::of_process(
            Box::new(ChildWriter {
                stdin: Some(stdin),
                child: Some(child),
                pid,
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
    child: Option<PipedChild>,
    pid: Option<u32>,
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
        // has no shutdown request, it exits when its input ends. The seam does
        // the rest of the ladder, over the server's own process group.
        drop(self.stdin.take());
        let Some(mut child) = self.child.take() else {
            return Departure {
                exit: Exit::Closed,
                pid: self.pid,
            };
        };
        let exit = match child.stop().await {
            PipedExit::Closed => Exit::Closed,
            PipedExit::Code(code) => Exit::Code(code),
            PipedExit::Killed => Exit::Killed,
            PipedExit::Unknown => Exit::Unknown,
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
