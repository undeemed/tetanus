//! A child this process talks to, rather than one it waits for.
//!
//! [`crate::proc`] runs a command to completion and hands back what it
//! printed. That is the wrong shape for a protocol peer: an MCP server, an
//! out-of-process hook, a language server - each is a long conversation with a
//! program that stays up, where stdout is the wire and closing stdin is how it
//! is told to go home. This is that shape, and it is here rather than in each
//! consumer for one reason worth stating plainly: everything that leaves the
//! harness should leave through one seam, or the guarantees the seam makes are
//! true of some children and not others.
//!
//! **The guarantee that was missing.** A consumer spawning its own child kills
//! *that child*. A protocol peer that starts helpers of its own - a language
//! server's indexer, a server that shells out - leaves them behind, holding
//! their pipes, sometimes for hours. Every child here leads its own process
//! group and is ended with the same SIGTERM-to-SIGKILL ladder over that group
//! that [`crate::proc`] uses, so the peer's own children go with it.
//!
//! **Stopping is polite first.** A protocol peer on stdio has no shutdown
//! request: it exits when its input ends. So the ladder starts before the
//! signals - close stdin, wait out the grace - and only then reaches for the
//! group. A peer that exits on its own is never signalled at all.
//!
//! **The environment is what the caller listed.** As everywhere in this crate:
//! a peer named in a settings document is a program a deployment chose, and
//! handing it every credential this process holds is a decision nobody made.
//!
//! **stderr is the operator's, stdout is the protocol's.** A peer's diagnostics
//! are inherited so they land beside the harness's own, and stdout is piped
//! always - a peer whose protocol stream was inherited would print its frames
//! onto the terminal and answer nobody.
//!
//! Parity: upstream's stdio transport in `packages/mcp` and the piped half of
//! `packages/subprocess/subprocess-local`.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, ChildStdin, ChildStdout};

/// How a peer's own diagnostics are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diagnostics {
    /// They land where the harness's do. The default, and what an operator
    /// debugging a peer needs.
    Inherit,
    /// They are discarded, for a peer whose chatter would drown the harness.
    Discard,
}

/// How a conversation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipedExit {
    /// There was nothing left to stop: it had already been stopped.
    Closed,
    /// It exited on its own once its input closed, with this status.
    Code(i32),
    /// It outlasted its grace period and its process group was ended.
    Killed,
    /// It ended and the platform gave no status - a signal, usually.
    Unknown,
}

/// A peer this harness may start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipedCommand {
    pub program: String,
    pub args: Vec<String>,
    /// The child's whole environment. Empty means empty.
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    /// How long the peer gets to exit after its input is closed, before its
    /// process group is ended.
    pub grace: Duration,
    pub diagnostics: Diagnostics,
}

impl PipedCommand {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            grace: Duration::from_secs(2),
            diagnostics: Diagnostics::Inherit,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn envs<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env.extend(
            vars.into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
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

    pub fn diagnostics(mut self, diagnostics: Diagnostics) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Start it, and keep the pipes.
    pub fn spawn(&self) -> io::Result<PipedChild> {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&self.args)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(match self.diagnostics {
                Diagnostics::Inherit => Stdio::inherit(),
                Diagnostics::Discard => Stdio::null(),
            })
            // The backstop for the paths that never reach `stop`: a panic, a
            // dropped handle, a process exiting.
            .kill_on_drop(true);
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }
        // Its own process group, so what it starts can be ended with it.
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn()?;
        Ok(PipedChild {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            pid: child.id(),
            child: Some(child),
            grace: self.grace,
        })
    }
}

/// One running peer: its two pipes, and the way to end it.
#[derive(Debug)]
pub struct PipedChild {
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    pid: Option<u32>,
    /// Taken by the first [`PipedChild::stop`], so a second has nothing left
    /// to do.
    child: Option<Child>,
    grace: Duration,
}

impl PipedChild {
    /// The peer's process id, which is also its process-group id.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Take the writing half. The caller owns the framing; this seam owns the
    /// process.
    pub fn stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    /// Take the reading half.
    pub fn stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    /// Close the peer's input if it is still held here, which is how a peer on
    /// stdio is told there is nothing more coming.
    ///
    /// A caller that took [`PipedChild::stdin`] closes it by dropping it; this
    /// is for the rest.
    pub fn close_input(&mut self) {
        drop(self.stdin.take());
    }

    /// End the conversation and do not return until the peer is gone.
    ///
    /// Idempotent: a second call answers [`PipedExit::Closed`] rather than
    /// signalling something that may by now be a different process with the
    /// same number.
    pub async fn stop(&mut self) -> PipedExit {
        self.close_input();
        let Some(mut child) = self.child.take() else {
            return PipedExit::Closed;
        };
        match tokio::time::timeout(self.grace, child.wait()).await {
            Ok(Ok(status)) => status.code().map_or(PipedExit::Unknown, PipedExit::Code),
            // Waiting failed, which leaves no way to learn the status. The
            // group is still ended below: an unknown state is not a reason to
            // walk away from a running child.
            Ok(Err(_)) => {
                self.end_group(child).await;
                PipedExit::Unknown
            }
            Err(_elapsed) => {
                self.end_group(child).await;
                PipedExit::Killed
            }
        }
    }

    /// The ladder, over the group rather than the process: a peer that started
    /// helpers of its own does not leave them holding their pipes.
    async fn end_group(&self, mut child: Child) {
        let group = self.pid;
        crate::proc::terminate_group(group, self.grace, async move {
            // `wait` reaps the leader, so a caller looking for the pid
            // afterwards finds nothing rather than a zombie.
            let _ = child.wait().await;
        })
        .await;
    }
}
