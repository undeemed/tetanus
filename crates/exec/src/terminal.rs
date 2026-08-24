//! A persistent terminal: a shell on a real pseudo-terminal that a turn drives
//! one send at a time, with a bounded scrollback it can page back through.
//!
//! [`crate::session`] already keeps a shell alive between tool calls, so what
//! this adds is everything that needs a *terminal* underneath rather than a
//! pipe: a program that only runs when it believes it has one, a viewport of
//! what changed since the last send, pages of retained history, a `^C` that
//! reaches the command instead of the shell, and input that is typed rather
//! than fed as a script. Upstream's `terminal_*` tools are these; this is the
//! layer beneath them.
//!
//! **Readiness is announced, not guessed.** Upstream watches its terminal for
//! silence and decides a command has probably finished. This tells the shell
//! to print a marker before every prompt ([`crate::sanitize`]), so a send
//! settles when the shell says the command is over - and the marker carries
//! the exit status, which silence never could. The guess is still here as the
//! fallback it should have been: a program that prints no marker (a REPL, a
//! pager, a password prompt) settles on silence, and both are named in the
//! answer so a caller knows which one it got.
//!
//! **A send never waits for ever.** Three bounds end one: the marker, silence,
//! and an absolute deadline. A fourth ends it from outside - the turn's
//! interrupt, which sends `SIGINT` to the terminal's foreground group, the way
//! a person pressing `^C` would, leaving the shell alive to answer the next
//! send.
//!
//! **One send at a time.** Two commands typed at one terminal interleave into
//! one stream nobody can attribute, so a second send while one is running is
//! refused rather than queued: the caller is told, which is what upstream's
//! `SEND_ACTIVE` says too.
//!
//! **A send that did not settle on a prompt left a command running**, and that
//! command still owes a prompt. The interrupt path waits for it, because it
//! just killed the thing that owed it; silence and the deadline cannot, since
//! the command is by definition still working. So a send after one of those
//! two can settle on the *earlier* command's prompt, with a viewport holding
//! whatever both printed. This is not hidden: the wait reason is how a caller
//! knows the session is still busy, and the honest next move is `terminal_read`
//! or a signal rather than another command. Upstream has the same hazard and
//! the same answer.
//!
//! Parity: upstream `packages/terminal/terminal-bash` (`session.ts`,
//! `config.ts`) and the session half of `packages/terminal/terminal`.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tetanus_turn::interrupt::Interrupt;

use crate::backend::{BackendError, ShellBackend};
use crate::pty::{PtyConfig, PtyError, PtySession};
use crate::sanitize::Sanitizer;
use crate::screen::Screen;
use crate::transcript::Transcript;

/// What a program on one of these terminals is told it is talking to.
///
/// Not `dumb`, which is the right answer for a pipe and the wrong one here:
/// this layer exists so a program that needs a terminal can have one, and the
/// first thing such a program does is ask what kind. `xterm-256color` is the
/// safe lingua franca - what a modern terminal emulator claims, and what
/// `crate::screen` models enough of to read back.
pub const TERM: &str = "xterm-256color";

/// How a terminal session is configured.
#[derive(Debug, Clone)]
pub struct TerminalConfig {
    /// Where the shell starts.
    pub cwd: PathBuf,
    pub rows: u16,
    pub cols: u16,
    /// Bytes of sanitized scrollback kept. Overflow drops the beginning,
    /// because the end is what a reader is reading.
    pub scrollback_bytes: usize,
    /// The most one read or one viewport may answer with.
    pub max_read_bytes: usize,
    /// Lines one read answers with when the caller names no count.
    pub default_read_lines: usize,
    /// How often readiness is re-examined while a send is waiting.
    pub poll: Duration,
    /// Silence that settles a send that never saw a prompt marker.
    pub idle_silence: Duration,
    /// The absolute bound on one send.
    pub timeout: Duration,
    /// How long the terminal's process group has between SIGTERM and SIGKILL.
    pub grace: Duration,
    /// Extra environment, over the backend's own.
    pub env: BTreeMap<String, String>,
    /// Variables taken from this process's environment where they are set.
    ///
    /// Nothing is inherited in this crate - a child gets what the caller
    /// listed - and for a one-shot command that is exactly right. A terminal
    /// session is where it stops being right on its own: the programs this
    /// layer exists to run are the interactive ones, and an interactive
    /// program with no `HOME` is a `git` with no configuration, an `ssh` with
    /// no keys, a `vim` that cannot write its own state file. Measured, not
    /// assumed: `vim` on a terminal with no `HOME` paints its status line and
    /// nothing else.
    ///
    /// So this is the same shape as `crate::hooks::HookEnv`: a list of names
    /// that pass, not a denylist of names that do not. An operator can read it
    /// and see that `PATH` reaches a session and `AWS_SECRET_ACCESS_KEY` does
    /// not.
    pub passed: Vec<String>,
    /// The kernel boundary the shell and everything it starts run behind.
    pub sandbox: tetanus_sandbox::Policy,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            cwd: cwd.clone(),
            // Upstream's terminal defaults: wide enough that a table or a
            // compiler diagnostic is not folded into nonsense.
            rows: 40,
            cols: 160,
            scrollback_bytes: 4 * 1024 * 1024,
            max_read_bytes: 256 * 1024,
            default_read_lines: 500,
            poll: Duration::from_millis(25),
            idle_silence: Duration::from_secs(3),
            timeout: Duration::from_secs(30),
            grace: Duration::from_secs(3),
            env: BTreeMap::new(),
            // What an interactive program cannot work without, and nothing
            // else. `TERM` is not here because this layer sets it itself.
            passed: ["PATH", "HOME", "LANG", "LC_ALL", "TZ", "USER", "SHELL"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            sandbox: tetanus_sandbox::Policy::danger_full_access(cwd),
        }
    }
}

/// Why one send stopped waiting.
///
/// Upstream's four, plus the one it has no equivalent of: a turn that was
/// stopped. They are reported rather than collapsed because a caller's next
/// move differs - a prompt means ask something else, silence means the program
/// may still be working, a timeout means it probably is, and an exit means
/// there is nothing left to ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// The shell announced a prompt: the command is over, and its status is
    /// known.
    StdinRead,
    /// Nothing has been printed for a while. The command may be waiting for
    /// input, or thinking.
    InferredIdle,
    /// The send's own deadline passed with the command still running.
    Timeout,
    /// The shell itself exited.
    SessionExit,
    /// The turn was stopped. The foreground group was interrupted; the session
    /// is still there.
    Interrupted,
}

impl std::fmt::Display for WaitReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            WaitReason::StdinRead => "stdin_read",
            WaitReason::InferredIdle => "inferred_idle",
            WaitReason::Timeout => "timeout",
            WaitReason::SessionExit => "session_exit",
            WaitReason::Interrupted => "interrupted",
        })
    }
}

/// Whether the shell on the terminal is still there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Running,
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Running => f.write_str("running"),
            Status::Exited { code, signal } => write!(
                f,
                "exited code={} signal={}",
                code.map(|code| code.to_string())
                    .unwrap_or_else(|| "null".into()),
                signal
                    .map(|signal| signal.to_string())
                    .unwrap_or_else(|| "null".into())
            ),
        }
    }
}

/// What one send produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendOutcome {
    /// Everything the terminal printed while this send was waiting, including
    /// the echo of what was typed, which is what a terminal shows.
    pub viewport: String,
    pub wait: WaitReason,
    pub status: Status,
    /// Whether the scrollback bound dropped part of this send's output.
    pub truncated: bool,
    /// What the command exited with, when a prompt marker said so.
    pub code: Option<i32>,
}

/// One page of retained output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub text: String,
    /// Lines the session still retains.
    pub total_lines: usize,
    /// Where this page begins, counted back from the newest line.
    pub line_begin: usize,
    /// Where it ends, on the same count.
    pub line_end: usize,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error(transparent)]
    Sandbox(#[from] tetanus_sandbox::SandboxError),
    #[error(transparent)]
    Pty(#[from] PtyError),
    /// The shell never reached its first prompt, so nothing was published.
    #[error("the {backend} terminal did not reach a prompt within {}ms; no session was opened", .after.as_millis())]
    NoPrompt {
        backend: &'static str,
        after: Duration,
    },
    /// The shell exited while it was starting.
    #[error("the {backend} terminal exited while it was starting up: {status}")]
    DiedStarting {
        backend: &'static str,
        status: Status,
    },
    /// Another send is still running on this terminal.
    #[error("terminal session {0:?} is already running a command; wait for it, or interrupt it")]
    SendActive(String),
    /// The session is closed or its shell is gone, and this is what happened.
    #[error("terminal session {id:?} is no longer running ({status}); open a new one")]
    Ended { id: String, status: Status },
    /// A page was asked for in a way that has no answer.
    #[error("{0}")]
    BadPage(String),
    /// No backend is registered under the type a caller asked for.
    #[error("no terminal backend is registered as {asked:?}; this deployment offers {}", listed(.registered))]
    NoBackend {
        asked: String,
        registered: Vec<String>,
    },
    /// Two backends claimed one type, which would make a request ambiguous.
    #[error("a terminal backend called {0:?} is already registered")]
    DuplicateBackend(String),
    /// This owner already has a session by that name.
    #[error(
        "{owner} already has a terminal session named {name:?}; close it, or choose another name"
    )]
    DuplicateName { owner: String, name: String },
    /// A name that names nothing.
    #[error("{0}")]
    BadName(String),
    /// No session has ever had this id.
    #[error("no terminal session {0:?}")]
    NoSession(String),
    /// The session exists and belongs to somebody else. Said plainly rather
    /// than as "no such session", because the boundary is between parts of one
    /// harness: a caller that reached for another owner's session has a bug to
    /// read, not a secret to keep.
    #[error("terminal session {0:?} belongs to another owner")]
    Foreign(String),
    /// A signal that would end the session was aimed at the shell itself.
    #[error("{signal} aimed at the shell would end terminal session {id:?}; close it instead, or signal it while a command is running")]
    WouldKillShell { id: String, signal: &'static str },
    /// A signal could not be delivered, most often because there is no longer
    /// a foreground group to deliver it to.
    #[error("could not signal terminal session {id:?}: {source}")]
    NotSignalled {
        id: String,
        #[source]
        source: PtyError,
    },
}

/// The registered backend types, for a refusal that says what is on offer.
fn listed(registered: &[String]) -> String {
    if registered.is_empty() {
        return "none at all".to_string();
    }
    registered.join(", ")
}

/// The signals a caller may deliver to a terminal.
///
/// A closed list rather than a number: a model that can name any signal can
/// name `SIGSTOP`, and a stopped shell nobody can continue is a session that
/// is neither alive nor gone. Upstream allows exactly these five.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSignal {
    Int,
    Term,
    Kill,
    Tstp,
    Hup,
}

impl TerminalSignal {
    pub const NAMES: [&'static str; 5] = ["SIGINT", "SIGTERM", "SIGKILL", "SIGTSTP", "SIGHUP"];

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "SIGINT" => Some(Self::Int),
            "SIGTERM" => Some(Self::Term),
            "SIGKILL" => Some(Self::Kill),
            "SIGTSTP" => Some(Self::Tstp),
            "SIGHUP" => Some(Self::Hup),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Int => "SIGINT",
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
            Self::Tstp => "SIGTSTP",
            Self::Hup => "SIGHUP",
        }
    }

    fn number(self) -> i32 {
        match self {
            Self::Int => libc::SIGINT,
            Self::Term => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
            Self::Tstp => libc::SIGTSTP,
            Self::Hup => libc::SIGHUP,
        }
    }

    /// Whether this signal would end the shell rather than a command it is
    /// running.
    fn ends_a_shell(self) -> bool {
        matches!(self, Self::Kill | Self::Term | Self::Hup)
    }
}

/// The sanitized transcript and the prompts seen in it, kept together because
/// a reader wants both at one instant: "what has been printed" and "has the
/// shell finished" are one question asked twice.
struct Watched {
    /// What a person looking at this terminal would see, which is a different
    /// question from what was printed on it and the only useful answer for a
    /// program that draws.
    screen: Screen,
    /// Whether the program on this terminal has just asked for a password.
    ///
    /// `sudo`'s two-state filter, over this crate's sanitized stream: every
    /// new chunk of output clears it, and a chunk whose last line looks like a
    /// credential prompt arms it again. What it guards is the *record* - a
    /// send made into an armed terminal is journalled redacted whether or not
    /// the model remembered to say so.
    prompting: AtomicBool,
    text: Arc<Transcript>,
    /// How many prompt markers the shell has printed since it started.
    prompts: AtomicUsize,
    /// How many of those are still owed by a command nobody waited for.
    ///
    /// A send that settled on anything but a prompt left a command running,
    /// and that command will print its prompt eventually - after this send
    /// answered. Counting the debt is what stops the *next* send settling on
    /// it: without this, a send would return instantly with an empty viewport
    /// and report a command it never ran as finished. It used to be a bounded
    /// wait at the end of the interrupt path, which is a race the machine wins
    /// whenever it is busy.
    owed: AtomicUsize,
    /// The status the last one carried, or `i64::MIN` for none yet.
    last_status: AtomicI64,
    /// When something was last printed, for the silence fallback.
    quiet_since: Mutex<Instant>,
}

impl Watched {
    fn new(bound: usize, rows: u16, cols: u16) -> Self {
        Self {
            screen: Screen::new(rows, cols),
            prompting: AtomicBool::new(false),
            text: Arc::new(Transcript::new(bound)),
            prompts: AtomicUsize::new(0),
            owed: AtomicUsize::new(0),
            last_status: AtomicI64::new(i64::MIN),
            quiet_since: Mutex::new(Instant::now()),
        }
    }

    fn idle_for(&self) -> Duration {
        self.quiet_since
            .lock()
            .expect("no panic holds this lock")
            .elapsed()
    }

    fn printed(&self) {
        *self.quiet_since.lock().expect("no panic holds this lock") = Instant::now();
    }
}

/// One persistent terminal.
pub struct TerminalSession {
    id: String,
    name: Option<String>,
    kind: String,
    backend: &'static str,
    cwd: PathBuf,
    pty: Arc<PtySession>,
    watched: Arc<Watched>,
    config: TerminalConfig,
    /// What the shell printed before its first prompt: its banner, and
    /// anything an operator's startup files had to say.
    motd: Mutex<String>,
    /// One send at a time, and a refusal rather than a queue for the second.
    sending: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

impl std::fmt::Debug for TerminalSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalSession")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("type", &self.kind)
            .field("status", &self.status())
            .finish()
    }
}

impl TerminalSession {
    /// Start a shell on a terminal and wait until it is asking for input.
    ///
    /// A session is published only once its shell has reached a prompt, for
    /// the reason upstream publishes only after setup succeeds: a session id
    /// that names a shell which never started is an id every later call fails
    /// on, and the caller has no way to tell that from a shell that died a
    /// moment later.
    pub async fn open(
        id: String,
        name: Option<String>,
        kind: String,
        backend: Arc<dyn ShellBackend>,
        config: TerminalConfig,
    ) -> Result<Self, TerminalError> {
        let resolved = backend.resolve()?;
        let mut env = backend.environment();
        for name in &config.passed {
            if let Ok(value) = std::env::var(name) {
                env.insert(name.clone(), value);
            }
        }
        // The backend's own default is `TERM=dumb`, which is right for a
        // command whose output is a pipe and wrong for everything this layer
        // exists to run: with `dumb` a program is being told there is no
        // screen, so `vim` refuses to start, `htop` exits, and every
        // full-screen program the terminal family was built for degrades to
        // the batch behaviour a pipe would already have given. A caller that
        // names its own `TERM` still wins.
        env.insert("TERM".to_string(), TERM.to_string());
        env.extend(backend.prompt_environment());
        env.extend(config.env.clone());
        let env: Vec<(String, String)> = env.into_iter().collect();

        let mut argv = vec![resolved.program().display().to_string()];
        argv.extend(backend.interactive());

        let confinement = Arc::new(tetanus_sandbox::prepare(&config.sandbox)?);
        let pty = Arc::new(
            PtySession::spawn_confined(
                &argv,
                &config.cwd,
                &env,
                PtyConfig {
                    rows: config.rows,
                    cols: config.cols,
                    // The raw terminal buffer holds escape sequences the
                    // sanitized one will not, so it is the wider of the two.
                    max_scrollback: config.scrollback_bytes * 2,
                    grace: config.grace,
                },
                Some(confinement),
            )
            .await?,
        );

        let watched = Arc::new(Watched::new(
            config.scrollback_bytes,
            config.rows,
            config.cols,
        ));
        tokio::spawn(sanitize_into(
            Arc::clone(&pty),
            Arc::clone(&watched),
            config.poll,
        ));

        let session = Self {
            id,
            name,
            kind,
            backend: backend.name(),
            cwd: config.cwd.clone(),
            pty,
            watched,
            motd: Mutex::new(String::new()),
            sending: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
            config,
        };
        session.reach_first_prompt(backend.name()).await?;
        // After the banner is taken, so what the shell said on the way up is
        // the session's `motd` and this crate's own housekeeping is not.
        for line in backend.terminal_setup() {
            session.send(&line, true, None).await?;
        }
        Ok(session)
    }

    /// The id its owner names it by.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The name its owner gave it, if any.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The backend type it was opened from.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Which shell it runs.
    pub fn backend(&self) -> &'static str {
        self.backend
    }

    /// Where the shell was started.
    pub fn opened_in(&self) -> &std::path::Path {
        &self.cwd
    }

    /// The shell's process id, which is also its process group.
    pub fn pid(&self) -> i32 {
        self.pty.leader()
    }

    /// What the shell printed before its first prompt.
    pub fn motd(&self) -> String {
        self.motd.lock().expect("no panic holds this lock").clone()
    }

    /// Whether the shell is still there.
    pub fn status(&self) -> Status {
        match self.pty.exit() {
            Some(exit) => Status::Exited {
                code: exit.code,
                signal: exit.signal,
            },
            None => Status::Running,
        }
    }

    /// Type something at the terminal and wait for the shell to answer.
    ///
    /// `submit` is the Enter key. It is separate from the text because a
    /// caller sending `\u{3}` or half a line of a REPL means to send exactly
    /// that: appending a newline for them would run something they did not
    /// write.
    pub async fn send(
        &self,
        text: &str,
        submit: bool,
        interrupt: Option<&Interrupt>,
    ) -> Result<SendOutcome, TerminalError> {
        self.send_waiting(text, submit, None, interrupt).await
    }

    /// [`TerminalSession::send`], waiting only as long as the caller asked.
    ///
    /// This is how work is started and left running. Upstream's answer to "run
    /// the build and come back later" is a background job with a job store
    /// behind it; a terminal needs neither, because the session *is* the
    /// collection point - a send that stops waiting leaves the command running
    /// on the terminal, [`TerminalSession::read`] collects what it has printed
    /// since, and [`TerminalSession::signal`] stops it. What was missing was a
    /// way to say so deliberately rather than by setting a deployment-wide
    /// timeout low and hoping.
    ///
    /// `within` is clamped by the deployment's own bound: a caller can ask to
    /// wait less, never more, for the reason every other cap in this crate
    /// exists.
    pub async fn send_waiting(
        &self,
        text: &str,
        submit: bool,
        within: Option<Duration>,
        interrupt: Option<&Interrupt>,
    ) -> Result<SendOutcome, TerminalError> {
        let Ok(_one_at_a_time) = self.sending.try_lock() else {
            return Err(TerminalError::SendActive(self.id.clone()));
        };
        if self.closed.load(Ordering::Acquire) || matches!(self.status(), Status::Exited { .. }) {
            return Err(TerminalError::Ended {
                id: self.id.clone(),
                status: self.status(),
            });
        }

        let from = self.watched.text.len();
        // Everything the shell owes from earlier sends has to arrive before a
        // marker means *this* command finished.
        let prompts_before = self.watched.prompts.load(Ordering::Acquire)
            + self.watched.owed.load(Ordering::Acquire);
        self.watched.printed();
        let typed = if submit {
            // Carriage return, not newline: a terminal in its ordinary mode
            // turns `\r` into the Enter key, and a `\n` is a line feed the
            // shell will not run.
            format!("{text}\r")
        } else {
            text.to_string()
        };
        if !typed.is_empty() {
            self.pty.write(&typed).await?;
        }
        // The window closes when the answer is submitted, as `sudo`'s does at
        // the newline: whatever the program prints next decides whether a new
        // one opens.
        if submit {
            self.watched.prompting.store(false, Ordering::Release);
        }

        let settled = self
            .wait_for_readiness(prompts_before, within, interrupt)
            .await;
        // A send that did not settle on a prompt left a command running, so
        // the shell now owes one more than it did. A send that *did* settle on
        // one has collected every marker up to and including its own, so the
        // debt is clear.
        match settled {
            WaitReason::StdinRead => self.watched.owed.store(0, Ordering::Release),
            WaitReason::SessionExit => {}
            _ => {
                self.watched.owed.fetch_add(1, Ordering::AcqRel);
            }
        }
        let snapshot = self.watched.text.snapshot();
        let seen = without_prompt_furniture(&snapshot.since(from));
        Ok(SendOutcome {
            viewport: bounded_tail(&seen, self.config.max_read_bytes).0,
            wait: settled,
            status: self.status(),
            truncated: snapshot.dropped > from,
            code: match settled {
                WaitReason::StdinRead => self.last_status(),
                // Only a prompt marker carries a status. Reporting the
                // previous command's under any other reason would be reporting
                // a number about the wrong command.
                _ => None,
            },
        })
    }

    /// One page of retained output, counted back from the newest line.
    ///
    /// Newest-relative because that is what a caller paging through history
    /// means: "the last 100 lines" stays the last 100 lines while the terminal
    /// keeps printing, where an offset from the beginning would slide.
    pub fn read(&self, offset: usize, count: Option<usize>) -> Result<Page, TerminalError> {
        let count = count.unwrap_or(self.config.default_read_lines);
        if count == 0 {
            return Err(TerminalError::BadPage(
                "a page of zero lines has nothing in it; ask for at least one".into(),
            ));
        }
        let snapshot = self.watched.text.snapshot();
        let dropped = snapshot.dropped > 0;
        let retained = snapshot.text();
        let lines: Vec<&str> = if retained.is_empty() {
            Vec::new()
        } else {
            retained.split('\n').collect()
        };
        let total_lines = lines.len();
        if offset >= total_lines {
            return Ok(Page {
                text: String::new(),
                total_lines,
                line_begin: offset,
                line_end: offset,
                truncated: dropped,
            });
        }
        let end = total_lines - offset;
        let start = end.saturating_sub(count);
        let (text, cut) = bounded_tail(&lines[start..end].join("\n"), self.config.max_read_bytes);
        let returned = if text.is_empty() {
            0
        } else {
            text.split('\n').count()
        };
        Ok(Page {
            text,
            total_lines,
            line_begin: offset,
            line_end: offset + returned,
            truncated: dropped || cut,
        })
    }

    /// What a person looking at this terminal would see right now.
    ///
    /// The answer for a program that *draws*: `htop` overwrites its cells
    /// rather than printing new ones, so its transcript is every frame at once
    /// and its screen is the one frame that is current.
    pub fn screen(&self) -> String {
        self.watched.screen.text()
    }

    /// Where the cursor is on that screen, which is how a reader knows which
    /// field a form is asking about.
    pub fn cursor(&self) -> crate::screen::Cursor {
        self.watched.screen.cursor()
    }

    /// Whether the program on this terminal has switched to the alternate
    /// screen.
    ///
    /// The most useful single bit this crate has about what a program is
    /// doing: entering it is how a program says, in the only way a terminal
    /// has, that it is drawing rather than printing. `vim`, `htop`, `less` and
    /// `git rebase -i` all do; `ls` and `cargo build` do not.
    pub fn is_drawing(&self) -> bool {
        self.watched.screen.is_alternate()
    }

    /// Tell the terminal it is a different shape, the way a terminal emulator
    /// does when its window is dragged.
    ///
    /// Not a tool: a model has no window, and a size it picked would be a
    /// number it made up. It is here for the surface that *does* have one - a
    /// presentation showing a live terminal has to be able to say how wide it
    /// is, or every full-screen program in the session draws for the wrong
    /// screen.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), TerminalError> {
        self.pty.resize(rows, cols)?;
        // The screen is the same shape as the terminal or it is describing a
        // different one: a program told to redraw for 24 rows would be read
        // back through a 40-row grid.
        self.watched.screen.resize(rows, cols);
        Ok(())
    }

    /// The size the terminal currently reports.
    pub fn size(&self) -> Result<(u16, u16), TerminalError> {
        Ok(self.pty.size()?)
    }

    /// Which process group owns the terminal now.
    ///
    /// Published because it is the difference between "the command is running"
    /// and "the shell is waiting", and a caller that has just interrupted
    /// something needs to know which it is looking at. `signal` asks the same
    /// question before it decides what a signal would hit.
    pub fn foreground_group(&self) -> Result<i32, TerminalError> {
        self.pty
            .foreground_group()
            .map_err(|source| TerminalError::NotSignalled {
                id: self.id.clone(),
                source,
            })
    }

    /// Interrupt what this terminal is running.
    ///
    /// Answers the foreground group it reached, because "the command was
    /// interrupted" and "the shell was interrupted" are different outcomes and
    /// a caller reporting one should not have to guess.
    ///
    /// **An interrupt goes to the shell as well, and that is the whole
    /// difference between this working and appearing to.** A shell running a
    /// *list* - a `for` loop, `a && b`, a script - forks each command as a job
    /// of its own, so the foreground group is one `sleep` out of a hundred.
    /// Signalling only that group kills that one `sleep`; the shell, which
    /// never saw the signal, starts the next one. The work continues and the
    /// caller has been told it was stopped.
    ///
    /// Measured, not reasoned: under load this case failed three times out of
    /// three with the loop still printing eight seconds later, and it passed
    /// on an idle machine *by accident* - because idle timing happened to
    /// catch the shell between jobs, which is the case where the shell does
    /// get the signal and does abandon the rest of the list.
    ///
    /// Only the interrupt class does this. `SIGTERM`, `SIGKILL` and `SIGHUP`
    /// stay on the foreground group, because a shell that received one of
    /// those would end the session - which is [`TerminalSession::close`], and
    /// a caller that meant it says so.
    pub fn signal(&self, signal: TerminalSignal) -> Result<i32, TerminalError> {
        let foreground =
            self.pty
                .foreground_group()
                .map_err(|source| TerminalError::NotSignalled {
                    id: self.id.clone(),
                    source,
                })?;
        // A signal that ends a shell, aimed at the shell, is a close written
        // as a signal - and one the caller would not be told about, because
        // the session would simply stop answering. Upstream refuses the same
        // shape.
        if signal.ends_a_shell() && foreground == self.pty.leader() {
            return Err(TerminalError::WouldKillShell {
                id: self.id.clone(),
                signal: signal.name(),
            });
        }
        self.pty
            .signal_group(foreground, signal.number())
            .map_err(|source| TerminalError::NotSignalled {
                id: self.id.clone(),
                source,
            })?;
        if signal == TerminalSignal::Int && foreground != self.pty.leader() {
            // Interactive bash ignores `SIGINT` at its own prompt, so a shell
            // that was between commands shrugs this off; a shell that was
            // working through a list abandons the rest of it, which is what
            // the caller asked for.
            let _ = self.pty.signal_group(self.pty.leader(), signal.number());
        }
        Ok(foreground)
    }

    /// End the session and everything on it, and wait until it is gone.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.pty.close().await;
    }

    /// Whether the owner has closed it.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Everything the terminal has printed that is still retained.
    pub fn scrollback(&self) -> String {
        self.watched.text.snapshot().text()
    }

    /// Whether the program on this terminal is asking for a password *now*.
    ///
    /// Read at record time by [`crate::terminal_tools`], which is why it is
    /// free of side effects: the same send is recorded three times - the
    /// streamed chunk, the assistant message, and the call - and all three
    /// have to agree.
    pub fn is_prompting_for_a_password(&self) -> bool {
        self.watched.prompting.load(Ordering::Acquire)
    }

    /// The status the last prompt marker carried.
    fn last_status(&self) -> Option<i32> {
        match self.watched.last_status.load(Ordering::Acquire) {
            i64::MIN => None,
            status => i32::try_from(status).ok(),
        }
    }

    /// Wait until the shell asks for input for the first time, and keep what
    /// it printed on the way as the session's banner.
    async fn reach_first_prompt(&self, backend: &'static str) -> Result<(), TerminalError> {
        let settled = self.wait_for_readiness(0, None, None).await;
        let banner = self.watched.text.snapshot().text();
        match settled {
            WaitReason::StdinRead | WaitReason::InferredIdle => {
                let banner = without_prompt_furniture(&banner);
                *self.motd.lock().expect("no panic holds this lock") =
                    bounded_tail(&banner, self.config.max_read_bytes).0;
                Ok(())
            }
            WaitReason::SessionExit => Err(TerminalError::DiedStarting {
                backend,
                status: self.status(),
            }),
            WaitReason::Timeout | WaitReason::Interrupted => Err(TerminalError::NoPrompt {
                backend,
                after: self.config.timeout,
            }),
        }
    }

    /// Watch the terminal until one of the four things that end a wait
    /// happens.
    async fn wait_for_readiness(
        &self,
        prompts_before: usize,
        within: Option<Duration>,
        interrupt: Option<&Interrupt>,
    ) -> WaitReason {
        let budget = within.map_or(self.config.timeout, |asked| asked.min(self.config.timeout));
        let deadline = Instant::now() + budget;
        loop {
            if self.watched.prompts.load(Ordering::Acquire) > prompts_before {
                return WaitReason::StdinRead;
            }
            // Asked after the marker, so a shell that printed its last prompt
            // and then exited settles as the prompt it was: the marker is
            // about the command, the exit is about the session.
            if self.pty.exit().is_some() {
                return WaitReason::SessionExit;
            }
            if interrupt.is_some_and(Interrupt::stopped) {
                // `^C`, aimed the way a person's is: at whatever owns the
                // terminal. A shell that was only waiting for input shrugs it
                // off, which is why the session survives its own interrupt.
                // The same two targets as `signal`, for the same reason: a
                // stopped turn has to stop the *work*, and killing one child
                // of a command list leaves the shell running the rest.
                if let Ok(foreground) = self.pty.foreground_group() {
                    let _ = self.pty.signal_group(foreground, libc::SIGINT);
                    if foreground != self.pty.leader() {
                        let _ = self.pty.signal_group(self.pty.leader(), libc::SIGINT);
                    }
                }
                return WaitReason::Interrupted;
            }
            // A caller waiting deliberately for a short time means the
            // deadline, not silence: a command that prints nothing for its
            // first half-second has not gone quiet, and answering
            // `inferred_idle` would tell the caller something this seam does
            // not know.
            if within.is_none() && self.watched.idle_for() >= self.config.idle_silence {
                return WaitReason::InferredIdle;
            }
            if Instant::now() >= deadline {
                return WaitReason::Timeout;
            }
            let stopping = async {
                match interrupt {
                    Some(interrupt) => interrupt.cancelled().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = self.watched.text.changed.notified() => {}
                _ = stopping => {}
                _ = tokio::time::sleep(self.config.poll) => {}
            }
        }
    }
}

/// Read the raw terminal, sanitize it, and publish what it means.
///
/// A task rather than work done on demand, for the reason the pty's own reader
/// is one: a transcript assembled only when someone asks would have to
/// re-scan, and a stateful sanitizer cannot re-scan - it has already consumed
/// the bytes that told it where it was.
async fn sanitize_into(pty: Arc<PtySession>, watched: Arc<Watched>, poll: Duration) {
    let mut sanitizer = Sanitizer::new();
    let mut cursor = 0usize;
    loop {
        let (raw, end) = pty.since(cursor);
        if !raw.is_empty() {
            cursor = end;
            // The screen is fed the raw bytes, escapes and all - they are the
            // drawing instructions, and the sanitizer's whole job is to throw
            // them away. Two models of one stream, neither derivable from the
            // other.
            answer(&pty, watched.screen.feed(&raw)).await;
            publish(&watched, sanitizer.push(&raw));
        }
        if pty.exit().is_some() {
            // The process is gone; the terminal may still hold what it printed
            // last. `wait` returns after the read loop has seen the terminal
            // close, which is after the last byte.
            pty.wait().await;
            let (raw, _) = pty.since(cursor);
            if !raw.is_empty() {
                answer(&pty, watched.screen.feed(&raw)).await;
                publish(&watched, sanitizer.push(&raw));
            }
            let last = sanitizer.flush();
            if !last.is_empty() {
                watched.text.push(&last);
                watched.printed();
            }
            return;
        }
        pty.changed(poll).await;
    }
}

/// Write back whatever the program asked the terminal for.
///
/// A failure is dropped rather than raised: the session is a terminal, and a
/// terminal whose reply could not be written is one whose program has gone -
/// which the drain loop is about to notice by itself.
async fn answer(pty: &Arc<PtySession>, replies: Vec<String>) {
    for reply in replies {
        let _ = pty.write(&reply).await;
    }
}

/// Record one sanitized chunk: the text, when it arrived, and any prompt the
/// shell announced in it.
fn publish(watched: &Arc<Watched>, chunk: crate::sanitize::Sanitized) {
    if !chunk.text.is_empty() {
        // `sudo`'s order, and the order matters: any new output ends the
        // previous password window before this chunk is asked whether it opens
        // one. A prompt that has been answered, or scrolled past, is over.
        watched.prompting.store(
            tetanus_turn::tools::looks_like_a_password_prompt(&chunk.text),
            Ordering::Release,
        );
        watched.text.push(&chunk.text);
        watched.printed();
    }
    if chunk.prompts.is_empty() {
        return;
    }
    for status in &chunk.prompts {
        watched
            .last_status
            .store(status.map(i64::from).unwrap_or(i64::MIN), Ordering::Release);
    }
    // Published last, and after the text, so a reader that saw the count grow
    // is a reader whose viewport already holds everything the command printed.
    watched
        .prompts
        .fetch_add(chunk.prompts.len(), Ordering::AcqRel);
    watched.printed();
    watched.text.changed.notify_waiters();
}

/// One send's output without the prompts around it.
///
/// A viewport spans from just after one prompt to just after the next, so its
/// two ends carry the terminal's furniture rather than the command's output -
/// and *which* end depends on whether the shell had printed `PS1` yet when the
/// last send settled, which is a race. Trimming both ends makes one command's
/// viewport the same text every time it is run. Upstream keeps the furniture,
/// and its own cases work around the raggedness; anything a program printed
/// that happens to look like a prompt is untouched, because only the two edges
/// are examined.
fn without_prompt_furniture(text: &str) -> String {
    let head = text
        .strip_prefix(crate::sanitize::PROMPT_TEXT)
        .unwrap_or(text);
    let tail = head
        .strip_suffix(crate::sanitize::PROMPT_TEXT)
        .unwrap_or(head);
    tail.trim_end_matches('\n').to_string()
}

/// The last `max_bytes` of `text`, cut on a character boundary, and whether
/// anything was cut.
fn bounded_tail(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let from = text.len() - max_bytes;
    let at = (from..=text.len())
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(text.len());
    (text[at..].to_string(), true)
}
