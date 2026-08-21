//! Running one external command, bounded in output and in time.
//!
//! This is the primitive under everything that has to leave the process: a
//! shell tool, an out-of-process hook, an MCP server on stdio, a subagent
//! driver. It is deliberately not a tool and registers nothing, so nothing a
//! model says can reach it yet.
//!
//! **The child's environment is what the caller listed, and nothing else.**
//! Upstream inherits the parent environment and removes what looks sensitive,
//! because Node hands a child `process.env` by default. Rust does not force
//! that choice, so this takes the other one: a denylist has to recognise every
//! secret a deployment might have set, and the one it does not recognise -
//! `ACME_DEPLOY_TOKEN_V2`, say - is handed to a subprocess that a model asked
//! to run. An allowlist fails the other way, which is the way worth failing.
//! [`Command::inherit_env`] exists for a caller that genuinely wants the
//! ambient environment, and its name is the warning.
//!
//! **Output is bounded, and the bound keeps the tail.** A command that prints
//! a gigabyte must not become a gigabyte of resident memory, and when
//! something is dropped it is the beginning: the end of a stream is where the
//! error message and the exit summary are.
//!
//! **A timeout is an outcome, not a failure.** A command that ran for its
//! whole budget and was killed still produced whatever it printed first, and
//! that output is usually the most useful thing anyone will get. It comes back
//! as an [`Output`] that says it timed out, rather than as an error that
//! throws the output away.
//!
//! **What this does not do yet**, stated rather than implied: it terminates
//! the child it started, not the process group. A command that spawns
//! grandchildren and exits leaves them running, and stopping that needs a
//! process-group call this workspace has no dependency for. Upstream's
//! `subprocess-local` does the full SIGTERM-to-SIGKILL escalation over a
//! process group; `docs/parity.md` carries the gap.
//!
//! Parity: upstream `packages/subprocess`, the collected-output half of its
//! `spawn.spec.ts`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

/// How much of a stream to keep, and how long to wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes kept per stream. Overflow drops from the front.
    pub max_capture: usize,
    /// How long the command may run before it is killed.
    pub timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // Enough for a build log's tail without being enough to hurt.
            max_capture: 64 * 1024,
            timeout: Duration::from_secs(120),
        }
    }
}

/// One captured stream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Captured {
    pub text: String,
    /// Whether anything was dropped to fit the bound. A reader that shows the
    /// text without this is telling someone a truncated log is the whole log.
    pub truncated: bool,
}

/// What a finished command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The exit code, or `None` when a signal ended it - including the kill a
    /// timeout sends.
    pub code: Option<i32>,
    pub stdout: Captured,
    pub stderr: Captured,
    /// Whether the command was still running when its budget ran out.
    pub timed_out: bool,
}

impl Output {
    /// Whether the command reported success. A timeout never does, whatever
    /// the kill happened to produce as a code.
    pub fn ok(&self) -> bool {
        !self.timed_out && self.code == Some(0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// The command could not be started at all: no such program, a working
    /// directory that is not there, no permission to execute. Distinct from a
    /// command that ran and failed, because the caller's next move differs -
    /// one is a broken request, the other is a result to read.
    #[error("could not start {program:?}: {source}")]
    NotStarted {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{program:?} could not be waited on: {source}")]
    Lost {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

/// One external command, fully specified.
#[derive(Debug, Clone)]
pub struct Command {
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
    stdin: Option<String>,
    limits: Limits,
}

impl Command {
    /// A command with no arguments, no environment and the default limits.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            stdin: None,
            limits: Limits::default(),
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

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Give the child one environment variable.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Give the child this process's whole environment.
    ///
    /// Named to be read twice. Everything this process was started with -
    /// every credential, every token - is handed to a program that may have
    /// been chosen by a model. Prefer naming what the child needs.
    pub fn inherit_env(mut self) -> Self {
        self.env.extend(std::env::vars());
        self
    }

    /// Write this to the child's standard input, then close it.
    ///
    /// Without it the child's input is empty and closed rather than left open,
    /// so a program that reads until end-of-file finishes instead of waiting
    /// for a budget it was never going to be given.
    pub fn stdin(mut self, data: impl Into<String>) -> Self {
        self.stdin = Some(data.into());
        self
    }

    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Run it, and wait for what it produced.
    pub async fn run(&self) -> Result<Output, ProcessError> {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&self.args)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The child is not put in the parent's process group by default on
            // any platform this targets, but killing it must not depend on
            // that: see the module note on what termination does and does not
            // reach.
            .kill_on_drop(true);
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }

        let mut child = command.spawn().map_err(|source| ProcessError::NotStarted {
            program: self.program.clone(),
            source,
        })?;

        // Taken before the wait, because the child cannot finish while a pipe
        // it is writing to is full and nobody is reading.
        let mut input = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let feeding = async {
            if let Some(handle) = input.as_mut() {
                if let Some(data) = &self.stdin {
                    // A child that exits without reading closes the pipe, and
                    // writing to it then fails. That is the child's choice and
                    // not this call's failure.
                    let _ = handle.write_all(data.as_bytes()).await;
                }
            }
            // Dropping it closes the pipe, which is what a reader waits for.
            drop(input.take());
        };

        let bound = self.limits.max_capture;
        let collecting = async {
            let (out, err) = tokio::join!(collect(stdout, bound), collect(stderr, bound));
            (out, err)
        };

        let waiting = async {
            let (_, (out, err)) = tokio::join!(feeding, collecting);
            let status = child.wait().await;
            (status, out, err)
        };

        match tokio::time::timeout(self.limits.timeout, waiting).await {
            Ok((status, stdout, stderr)) => {
                let status = status.map_err(|source| ProcessError::Lost {
                    program: self.program.clone(),
                    source,
                })?;
                Ok(Output {
                    code: status.code(),
                    stdout,
                    stderr,
                    timed_out: false,
                })
            }
            Err(_) => {
                // What it printed before the budget ran out is the useful part,
                // so the kill happens and the output still comes back.
                let _ = child.kill().await;
                Ok(Output {
                    code: None,
                    stdout: Captured::default(),
                    stderr: Captured::default(),
                    timed_out: true,
                })
            }
        }
    }
}

/// Read one stream to its end, keeping at most `bound` bytes of its tail.
async fn collect<R>(stream: Option<R>, bound: usize) -> Captured
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let Some(mut stream) = stream else {
        return Captured::default();
    };

    let mut kept: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => {
                kept.extend_from_slice(&chunk[..read]);
                if kept.len() > bound {
                    // Drop from the front: the end of a stream is where the
                    // error and the summary are.
                    let excess = kept.len() - bound;
                    kept.drain(..excess);
                    truncated = true;
                }
            }
            // A stream that stops being readable has given what it is going to
            // give; the exit status is the fact that matters after that.
            Err(_) => break,
        }
    }

    Captured {
        text: text_of(&kept, truncated),
        truncated,
    }
}

/// Turn captured bytes into text.
///
/// A tail cut at a byte bound lands mid-character routinely, and the leading
/// partial character is dropped rather than rendered as a replacement: a
/// reader should see a log that starts one character late, not one that starts
/// with a glyph the command never printed.
fn text_of(bytes: &[u8], truncated: bool) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(error) if truncated => {
            // Only a broken *prefix* is the cut's doing; anything later is the
            // command's own bytes and is rendered as best it can be.
            let start = error.valid_up_to();
            if start == 0 && error.error_len().is_some() {
                let skip = error.error_len().unwrap_or(1);
                String::from_utf8_lossy(&bytes[skip.min(bytes.len())..]).into_owned()
            } else {
                String::from_utf8_lossy(bytes).into_owned()
            }
        }
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}
