//! Running one external command: argv, environment, working directory,
//! captured stdio, exit status, signals, and a termination that reaches the
//! whole process group.
//!
//! This is the primitive under everything that has to leave the process: a
//! shell tool, a persistent shell, an out-of-process hook, an MCP server on
//! stdio, a subagent driver. It is deliberately not a tool and registers
//! nothing, so nothing a model says can reach it by itself.
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
//! **Termination is group-scoped.** Every child is spawned as the leader of
//! its own process group, and the kill a timeout sends is a SIGTERM to that
//! group, a grace period, then a SIGKILL to the group. A command that starts
//! grandchildren and a command that traps SIGTERM both end; neither leaves a
//! process behind holding the pipe this call is reading. Upstream's
//! `subprocess-local` runs the same ladder for the same reason.
//!
//! Parity: upstream `packages/subprocess`, `spawn.spec.ts` and
//! `process-exit.spec.ts`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;

use tetanus_core::spill::{SpillSource, SpillStore, SpillWriter};
use tetanus_sandbox::{Confinement, Enforcement};
use tetanus_turn::interrupt::Interrupt;

/// How much of a stream to keep, how long to wait, and how long a killed
/// process group gets to die politely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes kept per stream. Overflow drops from the front.
    pub max_capture: usize,
    /// How long the command may run before its group is terminated.
    pub timeout: Duration,
    /// How long a terminated group has between SIGTERM and SIGKILL. It is also
    /// how long a finished command's streams are given to close before the
    /// group is swept: an orphan holding the pipe must not hold the turn.
    pub grace: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // Enough for a build log's tail without being enough to hurt.
            max_capture: 64 * 1024,
            timeout: Duration::from_secs(120),
            // Upstream's `DEFAULT_GRACE_MS`, which is OpenCode's too.
            grace: Duration::from_secs(3),
        }
    }
}

/// Which of a command's two streams a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One piece of output, delivered while the command is still running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub stream: Stream,
    pub text: String,
}

/// Somewhere incremental output goes as it arrives.
///
/// A command that runs for a minute and prints throughout must be readable
/// before it ends, or a caller watching it has nothing to show and cannot tell
/// a slow command from a wedged one. The captured [`Output`] is still returned
/// at the end; a sink is what makes the middle visible.
///
/// It takes `&self` because both streams are read at once, by two tasks: an
/// implementation that needs to mutate owns its own lock.
pub trait OutputSink: Send + Sync {
    fn chunk(&self, chunk: Chunk);
}

/// A sink that keeps every chunk, in arrival order. The obvious implementation,
/// used by a caller that wants the whole interleaving rather than the two
/// separated tails.
#[derive(Debug, Default)]
pub struct Collected {
    chunks: Mutex<Vec<Chunk>>,
}

impl Collected {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every chunk so far, in the order the streams produced them.
    pub fn chunks(&self) -> Vec<Chunk> {
        self.chunks
            .lock()
            .expect("no panic holds this lock")
            .clone()
    }

    /// Every chunk so far, concatenated - the interleaving a terminal shows.
    pub fn text(&self) -> String {
        self.chunks()
            .into_iter()
            .map(|chunk| chunk.text)
            .collect::<Vec<_>>()
            .join("")
    }
}

impl OutputSink for Collected {
    fn chunk(&self, chunk: Chunk) {
        self.chunks
            .lock()
            .expect("no panic holds this lock")
            .push(chunk);
    }
}

/// One captured stream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Captured {
    pub text: String,
    /// Whether anything was dropped to fit the bound. A reader that shows the
    /// text without this is telling someone a truncated log is the whole log.
    pub truncated: bool,
    /// Where the whole stream was kept, when the caller asked for a spill and
    /// the bound dropped something. `None` covers three different things and
    /// they all read the same to a caller: nothing was dropped, nobody asked,
    /// or the spill itself failed - and a failed spill is never a failed
    /// command.
    pub spilled: Option<String>,
}

/// Why a command stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// It ran to completion, whatever its exit status was.
    Exited,
    /// It was still running when its budget ran out, and its group was killed.
    TimedOut,
    /// The caller asked the turn to stop, and its group was killed.
    Interrupted,
}

/// What a finished command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The exit code, or `None` when a signal ended it - including the kill a
    /// timeout or an interrupt sends.
    pub code: Option<i32>,
    /// The signal that ended it, when one did. Named rather than numbered
    /// where the platform has a name for it, because `[killed by signal:
    /// SIGKILL]` is what a reader can act on.
    pub signal: Option<String>,
    pub stdout: Captured,
    pub stderr: Captured,
    /// How it ended.
    pub ending: Ending,
    /// Whether the process group had to be swept: the leader was gone but
    /// something it started was still there, holding a pipe open. A caller
    /// that reports this is telling the truth about what the command left
    /// behind, and the sweep is why the call returned at all.
    pub swept: bool,
}

impl Output {
    /// Whether the command reported success. A kill never does, whatever the
    /// killed process happened to produce as a status.
    pub fn ok(&self) -> bool {
        self.ending == Ending::Exited && self.code == Some(0)
    }

    /// Whether the command was still running when its budget ran out.
    pub fn timed_out(&self) -> bool {
        self.ending == Ending::TimedOut
    }

    /// Whether the caller's interrupt is what ended it.
    pub fn interrupted(&self) -> bool {
        self.ending == Ending::Interrupted
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
#[derive(Clone)]
pub struct Command {
    program: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
    stdin: Option<String>,
    limits: Limits,
    sink: Option<Arc<dyn OutputSink>>,
    /// The kernel boundary this command runs behind, prepared by the caller.
    /// `None` is a command nobody asked to confine; a policy that asked and
    /// could not be honoured never becomes a `Command` at all, because
    /// preparing it already failed.
    confinement: Option<Arc<Confinement>>,
    /// Where to keep the whole of a stream this command's bound would
    /// otherwise drop.
    spill: Option<Spill>,
}

/// Where a dropped stream is kept, and what to call it.
#[derive(Debug, Clone)]
pub struct Spill {
    pub store: Arc<SpillStore>,
    pub source: SpillSource,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .field("stdin", &self.stdin)
            .field("limits", &self.limits)
            .field("streaming", &self.sink.is_some())
            .finish()
    }
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
            sink: None,
            confinement: None,
            spill: None,
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

    /// Give the child several environment variables.
    pub fn envs<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env
            .extend(vars.into_iter().map(|(k, v)| (k.into(), v.into())));
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

    /// Deliver output to `sink` as it arrives, as well as capturing its tail.
    pub fn streaming(mut self, sink: Arc<dyn OutputSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// Run this command behind a prepared kernel boundary.
    ///
    /// The confinement is built by the caller, in the caller's process, where
    /// a kernel that cannot honour the policy is still something a human can
    /// be told about. By the time it reaches here every question has been
    /// answered and what is left is one descriptor to apply.
    pub fn confined(mut self, confinement: Arc<Confinement>) -> Self {
        self.confinement = Some(confinement);
        self
    }

    /// Keep the whole of any stream the capture bound drops.
    ///
    /// Only the producer can do this. A bounded capture drops its beginning
    /// while the command is still running, so by the time a result exists the
    /// dropped bytes are gone and nothing above this seam can spill them. The
    /// file is opened the first time a bound is exceeded and never before, so
    /// a command whose output fits costs no filesystem at all - and because
    /// the buffer still holds everything at that instant, what lands on disk
    /// is the complete stream rather than the part that arrived after somebody
    /// noticed.
    pub fn spilling(mut self, store: Arc<SpillStore>, source: SpillSource) -> Self {
        self.spill = Some(Spill { store, source });
        self
    }

    /// How completely this command's boundary is enforced, for a caller
    /// rendering what happened.
    pub fn enforcement(&self) -> Option<Enforcement> {
        self.confinement
            .as_ref()
            .filter(|confinement| confinement.confines())
            .map(|confinement| confinement.enforcement)
    }

    /// Run it, and wait for what it produced.
    pub async fn run(&self) -> Result<Output, ProcessError> {
        self.execute(None).await
    }

    /// Run it, and kill its process group if the turn is interrupted first.
    ///
    /// The interrupt lands at the step boundary for the loop, but a tool call
    /// already dispatched is where a command lives, and a command nobody is
    /// waiting for any more must not keep running: the answer will never be
    /// read, and the process would outlive the turn that asked for it.
    pub async fn run_watching(&self, interrupt: &Interrupt) -> Result<Output, ProcessError> {
        self.execute(Some(interrupt)).await
    }

    async fn execute(&self, interrupt: Option<&Interrupt>) -> Result<Output, ProcessError> {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&self.args)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A last resort only: the ladder below is what actually ends a
            // command, because dropping the handle reaches the child and not
            // what the child started.
            .kill_on_drop(true);
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }
        // The child leads its own process group, so one `killpg` reaches
        // everything it starts. Without this the child joins the harness's own
        // group and signalling that group would kill the harness too.
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(target_os = "linux")]
        confine(&mut command, self.confinement.as_ref())?;

        let mut child = command.spawn().map_err(|source| ProcessError::NotStarted {
            program: self.program.clone(),
            source,
        })?;

        // Remembered now, because a reaped child answers `id()` with `None`
        // and the sweep below happens after the leader is gone. The number
        // stays a usable group id for exactly as long as the group has a
        // member, which is the only time anything is signalled with it.
        let group = child.id();

        // Taken before the wait, because the child cannot finish while a pipe
        // it is writing to is full and nobody is reading.
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let feeding = feed(stdin, self.stdin.clone());
        let out = self.reader(stdout, Stream::Stdout);
        let err = self.reader(stderr, Stream::Stderr);

        let ending = self.settle(&mut child, interrupt).await;
        let status = match ending {
            Ending::Exited => None,
            // The command is being stopped, so the ladder runs before anything
            // is joined: a group still running is a group still writing.
            Ending::TimedOut | Ending::Interrupted => Some(self.terminate(&mut child, group).await),
        };
        let status = match status {
            Some(status) => status,
            None => child.wait().await,
        }
        .map_err(|source| ProcessError::Lost {
            program: self.program.clone(),
            source,
        })?;

        // The leader is gone. Its streams normally close with it; when they do
        // not, something it started is still holding them, and that orphan is
        // swept rather than allowed to hold this call open for ever.
        let mut swept = false;
        if !finished(&[&out.task, &err.task], self.limits.grace).await {
            swept = kill_group(group, Signal::Kill);
            let _ = finished(&[&out.task, &err.task], self.limits.grace).await;
        }
        feeding.abort();

        Ok(Output {
            code: status.code(),
            signal: signal_name(&status),
            stdout: out.tail.captured(),
            stderr: err.tail.captured(),
            ending,
            swept,
        })
    }

    /// Wait for the command, its budget, or the interrupt - whichever is first.
    async fn settle(
        &self,
        child: &mut tokio::process::Child,
        interrupt: Option<&Interrupt>,
    ) -> Ending {
        let stopping = async {
            match interrupt {
                Some(interrupt) => interrupt.cancelled().await,
                // A caller that passed no interrupt races the clock only.
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            // `wait` only reaps the leader; the readers still decide when the
            // output is complete.
            _ = child.wait() => Ending::Exited,
            _ = tokio::time::sleep(self.limits.timeout) => Ending::TimedOut,
            _ = stopping => Ending::Interrupted,
        }
    }

    /// SIGTERM to the group, a grace period, then SIGKILL to the group.
    ///
    /// The ladder is the difference between stopping a command and stopping
    /// what the command started. Its first rung is polite - a shell script
    /// that traps SIGTERM gets to clean up - and its second is not optional.
    async fn terminate(
        &self,
        child: &mut tokio::process::Child,
        group: Option<u32>,
    ) -> std::io::Result<std::process::ExitStatus> {
        kill_group(group, Signal::Term);
        if let Ok(status) = tokio::time::timeout(self.limits.grace, child.wait()).await {
            // The leader is down; anything it started shares its group and is
            // swept by the caller's check rather than left behind.
            return status;
        }
        kill_group(group, Signal::Kill);
        // The leader cannot survive SIGKILL, so this wait is bounded by the
        // kernel rather than by anything the command chose.
        child.wait().await
    }

    fn reader(
        &self,
        stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
        which: Stream,
    ) -> Reader {
        let tail = Arc::new(Tail::new(self.limits.max_capture, self.spill_for(which)));
        let sink = self.sink.clone();
        let task = tokio::spawn(collect(stream, Arc::clone(&tail), sink, which));
        Reader { tail, task }
    }
}

impl Command {
    /// This command's spill target for one stream.
    ///
    /// The stream is folded into the artifact's name because a command that
    /// overran on both produces two files, and "which of these is stderr" is
    /// not a question an operator should have to answer by reading them.
    fn spill_for(&self, which: Stream) -> Option<Spill> {
        let spill = self.spill.as_ref()?;
        let suffix = match which {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        };
        Some(Spill {
            store: Arc::clone(&spill.store),
            source: SpillSource {
                tool: format!("{}-{suffix}", spill.source.tool),
                ..spill.source.clone()
            },
        })
    }
}

/// One stream being read: the bounded tail it fills, and the task filling it.
struct Reader {
    tail: Arc<Tail>,
    task: JoinHandle<()>,
}

/// Write the command's input and close the pipe, on a task of its own: a child
/// that never reads must not stop this call from waiting on it.
fn feed(stdin: Option<tokio::process::ChildStdin>, data: Option<String>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut stdin = stdin;
        if let (Some(handle), Some(data)) = (stdin.as_mut(), data) {
            // A child that exits without reading closes the pipe, and writing
            // to it then fails. That is the child's choice and not this call's
            // failure.
            let _ = handle.write_all(data.as_bytes()).await;
        }
        // Dropping it closes the pipe, which is what a reader waits for.
        drop(stdin);
    })
}

/// Whether every reader finished within `grace`.
async fn finished(tasks: &[&JoinHandle<()>], grace: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + grace;
    for task in tasks {
        if tokio::time::timeout_at(deadline, wait_for(task))
            .await
            .is_err()
        {
            return false;
        }
    }
    true
}

/// Wait for a task to finish without consuming its handle.
async fn wait_for(task: &JoinHandle<()>) {
    while !task.is_finished() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The bounded tail of one stream, readable while it is still being written.
///
/// Shared rather than returned, because a killed command still printed
/// something and the caller must be able to read it whether or not the reader
/// task ever saw end-of-file.
struct Tail {
    bound: usize,
    spill: Option<Spill>,
    state: Mutex<TailState>,
}

#[derive(Default)]
struct TailState {
    kept: Vec<u8>,
    truncated: bool,
    /// Open once the bound has been exceeded, and only then.
    writing: Option<SpillWriter>,
    /// Where the whole stream went, once the writer has been closed.
    spilled: Option<String>,
}

impl Tail {
    fn new(bound: usize, spill: Option<Spill>) -> Self {
        Self {
            bound,
            spill,
            state: Mutex::new(TailState::default()),
        }
    }

    fn push(&self, bytes: &[u8]) {
        let mut state = self.state.lock().expect("no panic holds this lock");
        // Already spilling: this piece goes to the file before the buffer
        // decides what to forget.
        if let Some(writing) = state.writing.as_mut() {
            if let Err(refused) = writing.write(bytes) {
                tracing::warn!(%refused, "a command's output could not be spilled");
                state.writing = None;
            }
        }
        state.kept.extend_from_slice(bytes);
        if state.kept.len() <= self.bound {
            return;
        }
        // The first overflow, and the last moment the buffer holds the whole
        // stream: open the file here and everything before this point is kept.
        if state.writing.is_none() && state.spilled.is_none() {
            if let Some(spill) = &self.spill {
                match spill.store.open(&spill.source) {
                    Ok(mut writing) => match writing.write(&state.kept) {
                        Ok(()) => state.writing = Some(writing),
                        Err(refused) => {
                            tracing::warn!(%refused, "a command's output could not be spilled")
                        }
                    },
                    Err(refused) => {
                        tracing::warn!(%refused, "a command's output could not be spilled")
                    }
                }
            }
        }
        // Drop from the front: the end of a stream is where the error and
        // the summary are.
        let excess = state.kept.len() - self.bound;
        state.kept.drain(..excess);
        state.truncated = true;
    }

    fn captured(&self) -> Captured {
        let mut state = self.state.lock().expect("no panic holds this lock");
        if let Some(writing) = state.writing.take() {
            match writing.finish() {
                Ok(kept) => state.spilled = Some(kept.locator),
                // The bytes are on disk either way; what failed is the promise
                // that they are all there, so nothing is promised.
                Err(refused) => {
                    tracing::warn!(%refused, "a command's spilled output could not be closed")
                }
            }
        }
        Captured {
            text: text_of(&state.kept, state.truncated),
            truncated: state.truncated,
            spilled: state.spilled.clone(),
        }
    }
}

/// Read one stream to its end, keeping its bounded tail and handing each piece
/// to the sink as it arrives.
async fn collect<R>(
    stream: Option<R>,
    tail: Arc<Tail>,
    sink: Option<Arc<dyn OutputSink>>,
    which: Stream,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let Some(mut stream) = stream else {
        return;
    };

    // A chunk boundary lands mid-character routinely, so a partial character
    // waits here for the bytes that finish it rather than being delivered as a
    // glyph the command never printed.
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => {
                tail.push(&chunk[..read]);
                if let Some(sink) = &sink {
                    pending.extend_from_slice(&chunk[..read]);
                    if let Some(text) = whole_characters(&mut pending) {
                        sink.chunk(Chunk {
                            stream: which,
                            text,
                        });
                    }
                }
            }
            // A stream that stops being readable has given what it is going to
            // give; the exit status is the fact that matters after that.
            Err(_) => break,
        }
    }
    if let Some(sink) = &sink {
        if !pending.is_empty() {
            // Whatever is left at end-of-file is delivered as best it can be:
            // there are no more bytes coming to complete it.
            sink.chunk(Chunk {
                stream: which,
                text: String::from_utf8_lossy(&pending).into_owned(),
            });
            pending.clear();
        }
    }
}

/// Take the complete characters off the front of `pending`, leaving a partial
/// one behind for the next read.
fn whole_characters(pending: &mut Vec<u8>) -> Option<String> {
    let valid = match std::str::from_utf8(pending) {
        Ok(_) => pending.len(),
        Err(error) => error.valid_up_to(),
    };
    if valid == 0 {
        return None;
    }
    let rest = pending.split_off(valid);
    let text = String::from_utf8(std::mem::replace(pending, rest)).expect("valid up to here");
    Some(text)
}

/// Turn captured bytes into text.
///
/// A tail cut at a byte bound lands mid-character routinely, and the leading
/// partial character is dropped rather than rendered as a replacement: a
/// reader should see a log that starts one character late, not one that starts
/// with a glyph the command never printed.
fn text_of(bytes: &[u8], truncated: bool) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    // Only a broken *prefix* is the cut's doing, and the whole prefix is
    // dropped: a character cut in three leaves two continuation bytes, and
    // dropping one of them would still leave a glyph the command never
    // printed. Anything later is the command's own bytes and is rendered as
    // best it can be.
    let start = if truncated {
        bytes
            .iter()
            .position(|byte| !is_continuation(*byte))
            .unwrap_or(bytes.len())
    } else {
        0
    };
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// Whether a byte is the middle of a UTF-8 character rather than the start of
/// one: `10xxxxxx`.
fn is_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

/// Which rung of the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    Term,
    Kill,
}

/// Run the termination ladder against a process group somebody else is
/// waiting on: SIGTERM, a grace period, then SIGKILL.
///
/// A persistent shell is the caller this exists for. Its child is owned by the
/// task watching it exit, so the ladder cannot be a method on a `Child` here;
/// `exited` is how the caller says "it is down", and the answer says whether
/// the polite rung was enough.
pub async fn terminate_group<F>(group: Option<u32>, grace: Duration, exited: F) -> bool
where
    F: std::future::Future<Output = ()>,
{
    kill_group(group, Signal::Term);
    tokio::pin!(exited);
    if tokio::time::timeout(grace, &mut exited).await.is_ok() {
        return true;
    }
    kill_group(group, Signal::Kill);
    // Nothing survives SIGKILL, so the second wait is bounded by the kernel;
    // the grace here only stops a caller hanging on a group the platform
    // cannot signal at all.
    let _ = tokio::time::timeout(grace, exited).await;
    false
}

/// Deliver `signal` to the process group the spawned child leads.
///
/// Answers whether anything received it, which is how a sweep tells "there
/// were orphans" from "the group was already empty".
#[cfg(unix)]
fn kill_group(group: Option<u32>, signal: Signal) -> bool {
    let Some(pid) = group else {
        return false;
    };
    let number = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // Safety: `killpg` is a plain system call. The group id is the child's own
    // pid, because the spawn made the child its group leader, so this can
    // never reach the harness's own group. A group that is already gone
    // answers ESRCH, which is the `false` below and not an error worth
    // reporting.
    let delivered = unsafe { libc::killpg(pid as libc::pid_t, number) };
    delivered == 0
}

/// Without process groups there is only the child.
///
/// Stated rather than implied: on a platform this workspace does not signal
/// groups on, a command that starts grandchildren can leave them behind. The
/// unix path is the one that carries the guarantee, and `docs/parity.md` says
/// so.
#[cfg(not(unix))]
fn kill_group(group: Option<u32>, _signal: Signal) -> bool {
    let _ = group;
    false
}

/// The name of the signal that ended a process, where the platform has one.
#[cfg(unix)]
fn signal_name(status: &std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|number| match number {
        libc::SIGHUP => "SIGHUP".to_string(),
        libc::SIGINT => "SIGINT".to_string(),
        libc::SIGQUIT => "SIGQUIT".to_string(),
        libc::SIGKILL => "SIGKILL".to_string(),
        libc::SIGSEGV => "SIGSEGV".to_string(),
        libc::SIGPIPE => "SIGPIPE".to_string(),
        libc::SIGTERM => "SIGTERM".to_string(),
        other => format!("signal {other}"),
    })
}

#[cfg(not(unix))]
fn signal_name(_status: &std::process::ExitStatus) -> Option<String> {
    None
}

/// Apply a prepared boundary to the child, between `fork` and `exec`.
///
/// The hook runs in the forked child, so what it may do is narrow: no
/// allocation, no locks, no library code that might take one - after a fork in
/// a process with threads, anything another thread held at that instant is
/// held for ever. Restricting a thread is three system calls, which is exactly
/// what runs here.
///
/// A failure is returned rather than logged. `pre_exec` failing makes `spawn`
/// fail, so a boundary that could not be applied is a command that does not
/// run: the alternative is running a model's command unconfined while the
/// caller believes otherwise.
#[cfg(target_os = "linux")]
fn confine(
    command: &mut tokio::process::Command,
    confinement: Option<&Arc<Confinement>>,
) -> Result<(), ProcessError> {
    use std::os::fd::AsRawFd;

    let Some(confinement) = confinement else {
        return Ok(());
    };
    let Some(ruleset) = confinement.ruleset.as_ref() else {
        // An unconfined policy, which the caller named out loud.
        return Ok(());
    };
    let ruleset = ruleset.as_raw_fd();
    // Safety: the closure calls `prctl` and two Landlock syscalls and nothing
    // else - no allocation, no locks - which is the whole requirement for code
    // between `fork` and `exec`. The descriptor is owned by the `Confinement`
    // the caller holds for the length of the spawn.
    unsafe {
        command.pre_exec(move || tetanus_sandbox::landlock::restrict_this_thread(ruleset));
    }
    Ok(())
}
