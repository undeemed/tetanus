//! Persistent shells: one long-lived shell a turn reuses across tool calls,
//! with its working directory and its exported variables surviving between
//! them.
//!
//! **Why a session at all.** A one-shot command is a fresh shell every time,
//! so `cd build` and `export CC=clang` are forgotten before the next call and
//! the model has to re-state them in every command it writes. Upstream solves
//! it with a PTY behind `ctx.terminals`; this solves it with a shell reading
//! its commands from a pipe. What the two have in common is the only thing
//! that matters to a model: the same process answers the next call, so its
//! state is still there.
//!
//! **The command boundary is a marker, not a prompt.** A shell reading a pipe
//! prints no prompt to synchronise on, so each command is wrapped: print a
//! start marker, run the command, print an end marker with its exit status.
//! The markers carry a per-command nonce, so a command that prints something
//! marker-shaped cannot lie about its own exit status. Upstream wraps for the
//! same reason, because prompt detection in a PTY has the same problem.
//!
//! **A dead shell is reported, never silently restarted.** A session whose
//! shell exited - the model ran `exit`, or a command killed it, or its budget
//! ran out - answers every later call with what happened to it. Restarting
//! underneath would hand the model a shell in a state it did not create: the
//! directory it changed to is gone, the variables it exported are gone, and
//! the next command runs somewhere it did not choose while the transcript says
//! it succeeded. Upstream restarts and prints a notice; the notice is the part
//! worth keeping, so this keeps the notice and drops the restart.
//!
//! Parity: upstream `packages/terminal/terminal-bash/tests/session.spec.ts`,
//! `packages/terminal/terminal/tests/service.spec.ts`, and
//! `packages/shell/tool-bash-persistent`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::sync::{watch, Notify};

use crate::backend::{BackendError, Markers, ShellBackend};
use crate::proc::{terminate_group, Chunk, OutputSink, Stream};

/// How long to wait between transcript checks when nothing woke the waiter.
/// Upstream polls its PTY on the same interval.
const POLL: Duration = Duration::from_millis(25);

/// What a session is configured with.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Where the shell starts.
    pub cwd: PathBuf,
    /// The budget one command gets before the session is ended.
    pub timeout: Duration,
    /// Bytes of transcript kept. Overflow drops from the front, because the
    /// end of a transcript is the part a model is reading.
    pub max_scrollback: usize,
    /// How long the shell's process group has between SIGTERM and SIGKILL.
    pub grace: Duration,
    /// Extra environment for the shell, over the backend's own overrides.
    pub env: BTreeMap<String, String>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            // Upstream's persistent-bash default.
            timeout: Duration::from_secs(300),
            max_scrollback: 256 * 1024,
            grace: Duration::from_secs(3),
            env: BTreeMap::new(),
        }
    }
}

/// Why a session is no longer usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gone {
    /// The shell process ended on its own or was killed by something else.
    Exited {
        code: Option<i32>,
        signal: Option<String>,
    },
    /// A command outran its budget and the session was ended with it.
    TimedOut { after: Duration },
    /// The owner closed it.
    Closed,
}

impl std::fmt::Display for Gone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Gone::Exited {
                code: Some(code), ..
            } => write!(f, "the shell exited with code {code}"),
            Gone::Exited {
                signal: Some(signal),
                ..
            } => write!(f, "the shell was killed by {signal}"),
            Gone::Exited { .. } => write!(f, "the shell exited"),
            Gone::TimedOut { after } => write!(
                f,
                "a command ran past its {}ms budget, so the shell was ended with it",
                after.as_millis()
            ),
            Gone::Closed => write!(f, "the session was closed"),
        }
    }
}

/// What one command in a session produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRun {
    /// Everything the command printed, stdout and stderr in the order the
    /// shell wrote them.
    pub text: String,
    /// The exit status the shell reported for it.
    pub code: i32,
    /// Whether the scrollback bound dropped part of this command's output.
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// The shell could not be started.
    #[error("could not start the {backend} session: {source}")]
    NotStarted {
        backend: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// The session is gone, and this is what happened to it. It is not
    /// restarted: a new one is [`ShellSessions::open`], by a caller that knows
    /// it is starting again from the workspace.
    #[error("shell session {id:?} is gone ({reason}); it was not restarted - open a new session")]
    Gone { id: String, reason: Gone },
    /// The shell ended while this command was running. What it printed first
    /// is carried, because that is usually why it died.
    #[error("the shell ended while the command was running ({reason}); it was not restarted")]
    Died { reason: Gone, partial: String },
    /// The command outran its budget. The session went with it, because a
    /// shell still running a command nobody is waiting for cannot be reused.
    #[error("the command ran past its {}ms budget; the session was ended with it", .after.as_millis())]
    TimedOut { after: Duration, partial: String },
    /// The shell stopped accepting input.
    #[error("the shell stopped accepting input: {0}")]
    Input(#[source] std::io::Error),
    #[error("no shell session named {0:?}")]
    Unknown(String),
}

impl SessionError {
    /// What the command printed before things went wrong, for a caller that
    /// has to tell a model what happened.
    pub fn partial(&self) -> Option<&str> {
        match self {
            SessionError::Died { partial, .. } | SessionError::TimedOut { partial, .. } => {
                Some(partial)
            }
            _ => None,
        }
    }
}

/// One long-lived shell.
///
/// Printed by its facts rather than its plumbing: an id, a backend, where it
/// started and whether it is still usable is what a caller debugging one
/// wants, and the pipe handles behind it are not printable anyway.
pub struct ShellSession {
    id: String,
    backend: Arc<dyn ShellBackend>,
    cwd: PathBuf,
    /// The shell's own process group: killing it reaches the shell and
    /// whatever it started.
    group: Option<u32>,
    config: SessionConfig,
    stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    transcript: Arc<Transcript>,
    /// Set once, by the task watching the shell exit.
    exited: watch::Receiver<Option<Gone>>,
    /// What ended the session, once something has. Distinct from `exited`
    /// because a closed session is gone before its process has finished
    /// dying.
    life: Mutex<Option<Gone>>,
    /// One command at a time. Two commands interleaved on one shell would
    /// interleave their markers, and neither could be attributed.
    running: tokio::sync::Mutex<()>,
    nonces: AtomicU64,
}

impl std::fmt::Debug for ShellSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellSession")
            .field("id", &self.id)
            .field("backend", &self.backend.name())
            .field("opened_in", &self.cwd)
            .field("gone", &self.gone())
            .finish()
    }
}

impl ShellSession {
    /// The id its owner names it by.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Which backend it runs.
    pub fn backend(&self) -> &'static str {
        self.backend.name()
    }

    /// Where the shell was started. Not where it is now: a session's current
    /// directory is the shell's, and only the shell can be asked.
    pub fn opened_in(&self) -> &Path {
        &self.cwd
    }

    /// Whether the session can still take a command, and why not when it
    /// cannot.
    pub fn gone(&self) -> Option<Gone> {
        if let Some(reason) = self.life.lock().expect("no panic holds this lock").clone() {
            return Some(reason);
        }
        self.exited.borrow().clone()
    }

    /// Everything the shell has printed that is still retained.
    pub fn transcript(&self) -> String {
        self.transcript.snapshot().text()
    }

    /// Run one command and wait for it to finish.
    pub async fn run(&self, command: &str) -> Result<SessionRun, SessionError> {
        self.run_with(command, None).await
    }

    /// [`ShellSession::run`], handing each piece of the command's own output to
    /// `sink` as the shell prints it.
    ///
    /// The markers are not delivered: they are this module's protocol, not the
    /// command's output, and a caller showing them to a model would be showing
    /// it the plumbing.
    pub async fn run_with(
        &self,
        command: &str,
        sink: Option<Arc<dyn OutputSink>>,
    ) -> Result<SessionRun, SessionError> {
        // One command at a time, so two callers cannot interleave markers on
        // one shell.
        let _one_at_a_time = self.running.lock().await;
        if let Some(reason) = self.gone() {
            return Err(SessionError::Gone {
                id: self.id.clone(),
                reason,
            });
        }

        let markers = Markers::new(&format!(
            "{}_{}",
            self.id.replace(|c: char| !c.is_ascii_alphanumeric(), ""),
            self.nonces.fetch_add(1, Ordering::Relaxed)
        ));
        let from = self.transcript.len();
        self.write(&format!("{}\n", self.wrap(command, &markers)))
            .await?;

        let deadline = tokio::time::Instant::now() + self.config.timeout;
        let mut delivered = from;
        loop {
            let woken = self.transcript.changed.notified();
            let snapshot = self.transcript.snapshot();
            let seen = &snapshot.since(from);

            if let Some(sink) = &sink {
                delivered = self.deliver(sink, &snapshot, delivered, &markers);
            }

            if let Some(complete) = between(seen, &markers) {
                return Ok(SessionRun {
                    text: complete.text,
                    code: complete.code,
                    truncated: snapshot.dropped > from,
                });
            }

            if let Some(reason) = self.exited.borrow().clone() {
                // The shell is gone and the end marker never came, so this
                // command is what ended it. What it printed first is the
                // evidence.
                self.end(reason.clone());
                return Err(SessionError::Died {
                    reason,
                    partial: partial(seen, &markers),
                });
            }

            if tokio::time::Instant::now() >= deadline {
                let after = self.config.timeout;
                self.close_now(Gone::TimedOut { after }).await;
                return Err(SessionError::TimedOut {
                    after,
                    partial: partial(seen, &markers),
                });
            }

            tokio::select! {
                _ = woken => {}
                _ = tokio::time::sleep(POLL) => {}
                _ = tokio::time::sleep_until(deadline) => {}
            }
        }
    }

    /// End the session and wait for its process group to be gone.
    ///
    /// Idempotent: closing a session that is already closed is not an error,
    /// because two owners racing to tidy up is not a fault.
    pub async fn close(&self) {
        self.close_now(Gone::Closed).await;
    }

    async fn close_now(&self, reason: Gone) {
        self.end(reason);
        // The pipe closing is what a shell reading its input treats as "no
        // more commands", so this is the polite rung before the signal.
        if let Some(stdin) = self.stdin.lock().await.take() {
            drop(stdin);
        }
        let mut exited = self.exited.clone();
        terminate_group(self.group, self.config.grace, async move {
            // The watcher publishes the exit; waiting for it is waiting for
            // the process to actually be gone rather than for a signal to have
            // been sent.
            while exited.borrow().is_none() {
                if exited.changed().await.is_err() {
                    return;
                }
            }
        })
        .await;
    }

    /// Record why the session is over, keeping the first reason: a shell that
    /// timed out and then exited was ended by the timeout.
    fn end(&self, reason: Gone) {
        let mut life = self.life.lock().expect("no panic holds this lock");
        if life.is_none() {
            *life = Some(reason);
        }
    }

    /// Hand the sink whatever is new, minus the markers.
    fn deliver(
        &self,
        sink: &Arc<dyn OutputSink>,
        snapshot: &Snapshot,
        delivered: usize,
        markers: &Markers,
    ) -> usize {
        let fresh = snapshot.since(delivered);
        if fresh.is_empty() {
            return delivered;
        }
        let text = strip_markers(&fresh, markers);
        if !text.is_empty() {
            sink.chunk(Chunk {
                stream: Stream::Stdout,
                text,
            });
        }
        snapshot.len()
    }

    fn wrap(&self, command: &str, markers: &Markers) -> String {
        self.backend.wrap(command, markers)
    }

    async fn write(&self, text: &str) -> Result<(), SessionError> {
        let mut stdin = self.stdin.lock().await;
        let Some(handle) = stdin.as_mut() else {
            return Err(SessionError::Gone {
                id: self.id.clone(),
                reason: self.gone().unwrap_or(Gone::Closed),
            });
        };
        handle
            .write_all(text.as_bytes())
            .await
            .map_err(SessionError::Input)?;
        handle.flush().await.map_err(SessionError::Input)
    }
}

/// One command's output, recovered from between its markers.
struct Complete {
    text: String,
    code: i32,
}

/// The output of a finished command, if its end marker has arrived.
fn between(seen: &str, markers: &Markers) -> Option<Complete> {
    let end = seen.rfind(&markers.end)?;
    let after = &seen[end + markers.end.len()..];
    let line = after.split('\n').next()?;
    // The status is only complete once its newline is in: a marker read
    // halfway would parse `1` out of `12`.
    if !after.contains('\n') {
        return None;
    }
    let code: i32 = line.trim().parse().ok()?;
    let body = match seen.rfind(&markers.start) {
        Some(start) => &seen[start + markers.start.len()..end],
        // The start marker was dropped by the scrollback bound; everything
        // retained belongs to this command anyway.
        None => &seen[..end],
    };
    Some(Complete {
        text: trim_edges(body),
        code,
    })
}

/// What a command printed before it was cut short.
fn partial(seen: &str, markers: &Markers) -> String {
    let body = match seen.rfind(&markers.start) {
        Some(start) => &seen[start + markers.start.len()..],
        None => seen,
    };
    trim_edges(&strip_markers(body, markers))
}

/// Drop the protocol's own lines out of a piece of transcript.
fn strip_markers(text: &str, markers: &Markers) -> String {
    let kept: Vec<&str> = text
        .split_inclusive('\n')
        .filter(|line| !line.contains(&markers.start) && !line.contains(&markers.end))
        .collect();
    kept.concat()
}

/// A command's output starts after the newline its start marker ended with and
/// finishes before the newline its end marker begins on.
fn trim_edges(body: &str) -> String {
    body.strip_prefix('\n')
        .unwrap_or(body)
        .trim_end_matches('\n')
        .to_string()
}

/// Everything a shell has printed, bounded, with a way to wait for more.
struct Transcript {
    bound: usize,
    state: Mutex<TranscriptState>,
    changed: Notify,
}

#[derive(Default)]
struct TranscriptState {
    kept: String,
    /// How many bytes the bound has dropped off the front, so a position in
    /// the transcript stays meaningful after a drop.
    dropped: usize,
}

/// The transcript as it stood at one moment.
struct Snapshot {
    kept: String,
    dropped: usize,
}

impl Snapshot {
    /// Everything from absolute position `from` onwards, as far as it is still
    /// retained.
    fn since(&self, from: usize) -> String {
        let start = from.saturating_sub(self.dropped).min(self.kept.len());
        // A position can land inside a character after a drop; the next
        // boundary is close enough and is always valid.
        let start = (start..=self.kept.len())
            .find(|at| self.kept.is_char_boundary(*at))
            .unwrap_or(self.kept.len());
        self.kept[start..].to_string()
    }

    fn len(&self) -> usize {
        self.dropped + self.kept.len()
    }

    /// The whole retained transcript.
    fn text(self) -> String {
        self.kept
    }
}

impl Transcript {
    fn new(bound: usize) -> Self {
        Self {
            bound,
            state: Mutex::new(TranscriptState::default()),
            changed: Notify::new(),
        }
    }

    fn push(&self, text: &str) {
        {
            let mut state = self.state.lock().expect("no panic holds this lock");
            state.kept.push_str(text);
            if state.kept.len() > self.bound {
                let excess = state.kept.len() - self.bound;
                let at = (excess..=state.kept.len())
                    .find(|at| state.kept.is_char_boundary(*at))
                    .unwrap_or(state.kept.len());
                state.kept.drain(..at);
                state.dropped += at;
            }
        }
        self.changed.notify_waiters();
    }

    fn snapshot(&self) -> Snapshot {
        let state = self.state.lock().expect("no panic holds this lock");
        Snapshot {
            kept: state.kept.clone(),
            dropped: state.dropped,
        }
    }

    fn len(&self) -> usize {
        let state = self.state.lock().expect("no panic holds this lock");
        state.dropped + state.kept.len()
    }
}

/// The sessions one owner has open.
///
/// Ids are minted here and are stable for the life of the session, because a
/// model has to be able to name the shell it opened two tool calls ago.
pub struct ShellSessions {
    open: Mutex<BTreeMap<String, Arc<ShellSession>>>,
    next: AtomicU64,
}

impl Default for ShellSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellSessions {
    pub fn new() -> Self {
        Self {
            open: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
        }
    }

    /// Start one shell and publish it under a fresh id.
    pub async fn open(
        &self,
        backend: Arc<dyn ShellBackend>,
        config: SessionConfig,
    ) -> Result<Arc<ShellSession>, SessionError> {
        let session = Arc::new(start(backend, config, self.mint()).await?);
        self.open
            .lock()
            .expect("no panic holds this lock")
            .insert(session.id.clone(), Arc::clone(&session));
        Ok(session)
    }

    /// The session with this id, whether or not it is still alive: a caller
    /// asking after a dead session is owed the reason, not "no such session".
    pub fn get(&self, id: &str) -> Result<Arc<ShellSession>, SessionError> {
        self.open
            .lock()
            .expect("no panic holds this lock")
            .get(id)
            .cloned()
            .ok_or_else(|| SessionError::Unknown(id.to_string()))
    }

    /// Every session this owner has, oldest id first.
    pub fn list(&self) -> Vec<Arc<ShellSession>> {
        self.open
            .lock()
            .expect("no panic holds this lock")
            .values()
            .cloned()
            .collect()
    }

    /// Close one session and forget it. Answers whether it was there.
    pub async fn close(&self, id: &str) -> Result<(), SessionError> {
        let session = self.get(id)?;
        session.close().await;
        self.open
            .lock()
            .expect("no panic holds this lock")
            .remove(id);
        Ok(())
    }

    /// Close everything. What a composition does on the way down, so a run
    /// that ends does not leave shells behind.
    pub async fn close_all(&self) {
        let all: Vec<Arc<ShellSession>> = self
            .open
            .lock()
            .expect("no panic holds this lock")
            .values()
            .cloned()
            .collect();
        for session in all {
            session.close().await;
        }
        self.open.lock().expect("no panic holds this lock").clear();
    }

    fn mint(&self) -> String {
        format!("shell-{}", self.next.fetch_add(1, Ordering::Relaxed))
    }
}

/// Start one shell, reading its commands from a pipe.
async fn start(
    backend: Arc<dyn ShellBackend>,
    config: SessionConfig,
    id: String,
) -> Result<ShellSession, SessionError> {
    let resolved = backend.resolve()?;
    let mut env = backend.environment();
    env.extend(config.env.clone());

    let mut command = tokio::process::Command::new(resolved.program());
    command
        .args(backend.session())
        .current_dir(&config.cwd)
        .env_clear()
        .envs(&env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(|source| SessionError::NotStarted {
        backend: backend.name(),
        source,
    })?;
    let group = child.id();
    let stdin = child.stdin.take();
    let transcript = Arc::new(Transcript::new(config.max_scrollback));

    // Both streams land in one transcript, in the order the reader saw them:
    // a model reading a session is reading a terminal, where a warning and the
    // line that provoked it belong next to each other. The backend also
    // redirects its own stderr onto stdout as it starts, so ordinary output
    // keeps the shell's own ordering exactly; anything the shell writes before
    // that lands here.
    for stream in [
        child.stdout.take().map(Readable::Out),
        child.stderr.take().map(Readable::Err),
    ]
    .into_iter()
    .flatten()
    {
        let transcript = Arc::clone(&transcript);
        tokio::spawn(async move {
            match stream {
                Readable::Out(handle) => pump(handle, transcript).await,
                Readable::Err(handle) => pump(handle, transcript).await,
            }
        });
    }

    let (tell, exited) = watch::channel(None);
    tokio::spawn(async move {
        let status = child.wait().await;
        let gone = match status {
            Ok(status) => Gone::Exited {
                code: status.code(),
                signal: signal_of(&status),
            },
            // A shell that cannot be waited on is a shell nobody can use
            // again, which is the same fact from the caller's side.
            Err(_) => Gone::Exited {
                code: None,
                signal: None,
            },
        };
        let _ = tell.send(Some(gone));
    });

    // Taken before the backend is moved into the session; it is what the shell
    // has to be told before it behaves like one.
    let setup = backend.setup();
    let session = ShellSession {
        id,
        cwd: config.cwd.clone(),
        group,
        config,
        stdin: tokio::sync::Mutex::new(stdin),
        transcript,
        exited,
        life: Mutex::new(None),
        running: tokio::sync::Mutex::new(()),
        nonces: AtomicU64::new(0),
        backend,
    };

    // Whatever the backend needs before it will behave like a session: bash
    // puts its own stderr on its stdout, so the transcript is in the order the
    // shell wrote it rather than in the order two pipes happened to be read.
    for line in setup {
        session.write(&format!("{line}\n")).await?;
    }
    Ok(session)
}

/// Which of the two pipes a reader task was handed.
enum Readable {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

/// Read one stream into the transcript, decoding as text arrives.
async fn pump<R>(mut stream: R, transcript: Arc<Transcript>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => {
                pending.extend_from_slice(&chunk[..read]);
                let valid = match std::str::from_utf8(&pending) {
                    Ok(_) => pending.len(),
                    Err(error) => error.valid_up_to(),
                };
                if valid > 0 {
                    let rest = pending.split_off(valid);
                    let text = String::from_utf8(std::mem::replace(&mut pending, rest))
                        .expect("valid up to here");
                    transcript.push(&text);
                }
            }
            Err(_) => break,
        }
    }
    if !pending.is_empty() {
        transcript.push(&String::from_utf8_lossy(&pending));
    }
}

/// The name of the signal that ended a process, where there is one.
#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|number| match number {
        libc::SIGTERM => "SIGTERM".to_string(),
        libc::SIGKILL => "SIGKILL".to_string(),
        libc::SIGHUP => "SIGHUP".to_string(),
        libc::SIGINT => "SIGINT".to_string(),
        other => format!("signal {other}"),
    })
}

#[cfg(not(unix))]
fn signal_of(_status: &std::process::ExitStatus) -> Option<String> {
    None
}
