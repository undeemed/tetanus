//! A real pseudo-terminal: the thing the terminal tools need and a pipe cannot
//! give.
//!
//! `docs/parity.md`'s terminal row names this as what the rest of that row
//! waits on, and the list is specific. A program behaves differently when its
//! output is a terminal - it colours, it pages, it draws a progress bar, it
//! asks for a password without echoing - and a model driving an interactive
//! program (`ssh`, `psql`, a REPL, `git rebase -i`) needs the program to be
//! talking to a terminal or it will not talk at all. A pipe cannot be resized,
//! has no foreground process group to signal, and reports no size for `stty`
//! to answer with.
//!
//! Four things this owns, because none of them can be built on top of the
//! others:
//!
//! **Allocation.** `posix_openpt` and its unlock dance, then a child that
//! `setsid`s and takes the slave as its controlling terminal. Without the
//! controlling-terminal step the child has a tty on its file descriptors and
//! still no session to signal, which is the subtle half-working state worth
//! avoiding.
//!
//! **Size, and resize.** A terminal has a size and programs read it. It is set
//! at allocation, so nothing starts up believing it has an 0x0 screen, and
//! changing it later delivers `SIGWINCH` the way a real terminal emulator does.
//!
//! **Signal delivery to the foreground group.** Ctrl-C on a terminal does not
//! go to "the process"; it goes to whichever process group currently owns the
//! terminal. Asking the master for that group is how an interrupt reaches the
//! command a shell is running rather than the shell itself, which is what lets
//! a session survive its own interrupt.
//!
//! **A read loop that does not lose output.** Two different losses are
//! possible and they need different answers. The kernel's own pty buffer
//! applies backpressure - a child writing into a full buffer blocks - so the
//! only way to lose bytes there is to stop reading, and this reads
//! continuously into memory rather than on demand. Our own buffer is bounded,
//! and when the bound is reached the beginning is dropped and the loss is
//! *reported*: a reader is told the transcript is not the whole story rather
//! than handed a shorter one that looks complete.
//!
//! Newline handling is the terminal's, not ours: a tty in its default mode
//! turns `\n` into `\r\n` on the way out, so a caller comparing against what a
//! program printed either normalizes or asks for raw mode. Saying so here is
//! cheaper than every caller discovering it.
//!
//! Parity: upstream `packages/terminal/terminal-bash` (its node-pty backend)
//! and the terminal half of `packages/subprocess/subprocess-local`.

#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::unix::AsyncFd;
use tokio::sync::watch;

use crate::transcript::Transcript;

/// How a terminal is set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyConfig {
    pub rows: u16,
    pub cols: u16,
    /// Bytes of transcript kept. Overflow drops the beginning and says so.
    pub max_scrollback: usize,
    /// How long the session's process group has between SIGTERM and SIGKILL.
    pub grace: Duration,
}

impl Default for PtyConfig {
    fn default() -> Self {
        Self {
            // A size a program will believe, rather than the 0x0 an
            // unconfigured pty reports and every `stty size` shows.
            rows: 24,
            cols: 80,
            max_scrollback: 256 * 1024,
            grace: Duration::from_secs(3),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("could not allocate a pseudo-terminal: {0}")]
    Allocate(#[source] std::io::Error),
    #[error("could not start {program:?} on a pseudo-terminal: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the terminal could not be {what}: {source}")]
    Terminal {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// There is no foreground process group to act on: the session has ended,
    /// or the kernel will not say. Distinct from a delivery failure because
    /// the caller's next move differs - one is "nothing to signal", the other
    /// is "the signal did not land".
    #[error("this terminal has no foreground process group to signal")]
    NoForeground,
}

/// How a terminal session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// One live pseudo-terminal and the process session on it.
///
/// Printed by its facts - which session, how big, whether it has ended - and
/// not by its descriptors, which are not printable and would say nothing
/// useful if they were.
pub struct PtySession {
    master: Arc<AsyncFd<Master>>,
    /// The child that leads the terminal's session; also its process-group id,
    /// because it called `setsid`.
    leader: i32,
    transcript: Arc<Transcript>,
    exited: watch::Receiver<Option<PtyExit>>,
    /// True once the read loop has seen the terminal close, so everything the
    /// session ever printed is in the transcript.
    drained: watch::Receiver<bool>,
    closing: AtomicBool,
    grace: Duration,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession")
            .field("leader", &self.leader)
            .field("size", &self.size().ok())
            .field("exit", &self.exit())
            .finish()
    }
}

impl PtySession {
    /// Allocate a terminal and start `argv` on it.
    pub async fn spawn(
        argv: &[String],
        cwd: &Path,
        env: &[(String, String)],
        config: PtyConfig,
    ) -> Result<Self, PtyError> {
        Self::spawn_confined(argv, cwd, env, config, None).await
    }

    /// [`PtySession::spawn`], behind a kernel boundary the caller prepared.
    ///
    /// The boundary is applied once, to the shell itself, for the reason
    /// [`crate::session`] applies it once to its own: everything the shell
    /// later starts is a child of a restricted process and inherits the
    /// restriction, which is what makes a session safe to keep open across
    /// tool calls. The confinement is prepared by the caller, in the caller's
    /// process, because the descriptor it holds has to outlive the spawn.
    pub async fn spawn_confined(
        argv: &[String],
        cwd: &Path,
        env: &[(String, String)],
        config: PtyConfig,
        confinement: Option<Arc<tetanus_sandbox::Confinement>>,
    ) -> Result<Self, PtyError> {
        let (master, slave_path) = allocate()?;
        set_size(master.as_raw_fd(), config.rows, config.cols)?;

        let program = argv.first().cloned().unwrap_or_default();
        // The slave is opened here and handed to the child as all three
        // standard descriptors. It is closed in this process immediately
        // afterwards: a slave still open on our side keeps the master readable
        // for ever, so the read loop would never see the child exit.
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&slave_path)
            .map_err(PtyError::Allocate)?;
        let slave_fd = slave.as_raw_fd();

        let mut command = std::process::Command::new(&program);
        command
            .args(argv.iter().skip(1))
            .current_dir(cwd)
            .env_clear()
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::from(slave.try_clone().map_err(PtyError::Allocate)?))
            .stdout(Stdio::from(slave.try_clone().map_err(PtyError::Allocate)?))
            .stderr(Stdio::from(slave.try_clone().map_err(PtyError::Allocate)?));
        // Safety: between `fork` and `exec` this calls two system calls and
        // nothing else - no allocation, no locks - which is the whole
        // requirement for a `pre_exec` hook. `setsid` makes the child a session
        // leader so it can take a controlling terminal, and `TIOCSCTTY` is it
        // taking one; without the pair the child has a terminal on its
        // descriptors but no session, and nothing can be signalled through it.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        // After the terminal is taken, never before: Landlock forbids nothing
        // this hook needs, but a boundary applied first would be one more
        // thing to reason about in the half of a fork that may not allocate.
        // Hooks run in the order they were added.
        if let Some(ruleset) = confinement
            .as_ref()
            .and_then(|confinement| confinement.ruleset.as_ref())
        {
            let ruleset = ruleset.as_raw_fd();
            // Safety: as in `crate::proc` - `prctl` and two Landlock syscalls,
            // no allocation and no locks, on a descriptor the caller holds for
            // the length of this spawn.
            unsafe {
                command.pre_exec(move || tetanus_sandbox::landlock::restrict_this_thread(ruleset));
            }
        }

        let child = command.spawn().map_err(|source| PtyError::Spawn {
            program: program.clone(),
            source,
        })?;
        drop(slave);
        let leader = child.id() as i32;

        let transcript = Arc::new(Transcript::new(config.max_scrollback));
        let master = Arc::new(AsyncFd::new(master).map_err(PtyError::Allocate)?);

        // Read continuously, not on demand. The kernel's pty buffer pushes
        // back on a child that outruns its reader, so a reader that only runs
        // when someone asks for output would throttle the child and, worse,
        // could deadlock a child that writes before it reads.
        let (drained_tell, drained) = watch::channel(false);
        tokio::spawn({
            let master = Arc::clone(&master);
            let transcript = Arc::clone(&transcript);
            async move {
                drain(master, transcript).await;
                // The terminal is closed and everything it held has been
                // collected: this is what makes `wait` a promise about output
                // and not only about the process.
                let _ = drained_tell.send(true);
            }
        });

        let (tell, exited) = watch::channel(None);
        std::thread::Builder::new()
            .name("tetanus-pty-wait".to_string())
            .spawn(move || {
                let exit = wait_for(child);
                let _ = tell.send(Some(exit));
            })
            .map_err(|source| PtyError::Spawn { program, source })?;

        Ok(Self {
            master,
            leader,
            transcript,
            exited,
            drained,
            closing: AtomicBool::new(false),
            grace: config.grace,
        })
    }

    /// The session leader's process id, which is also its process-group id.
    pub fn leader(&self) -> i32 {
        self.leader
    }

    /// Everything the terminal has printed that is still retained.
    pub fn transcript(&self) -> String {
        self.transcript.snapshot().text()
    }

    /// Everything printed since absolute position `from`, and where the
    /// transcript now ends.
    pub fn since(&self, from: usize) -> (String, usize) {
        let snapshot = self.transcript.snapshot();
        let end = snapshot.len();
        (snapshot.since(from), end)
    }

    /// Where the transcript currently ends, for a caller about to send
    /// something and read only what follows.
    pub fn mark(&self) -> usize {
        self.transcript.len()
    }

    /// Whether the bound has dropped anything at all.
    pub fn truncated(&self) -> bool {
        self.transcript.snapshot().dropped > 0
    }

    /// Wait until the transcript grows, or until `within` passes.
    pub async fn changed(&self, within: Duration) {
        let waiting = self.transcript.changed.notified();
        let _ = tokio::time::timeout(within, waiting).await;
    }

    /// Write to the terminal's input, exactly as typed - no newline is added,
    /// because a caller sending a control character means to send that and
    /// nothing else.
    pub async fn write(&self, data: &str) -> Result<(), PtyError> {
        let mut rest = data.as_bytes();
        while !rest.is_empty() {
            let mut guard = self
                .master
                .writable()
                .await
                .map_err(|source| PtyError::Terminal {
                    what: "written to",
                    source,
                })?;
            match guard.try_io(|inner| write_fd(inner.get_ref().as_raw_fd(), rest)) {
                Ok(Ok(written)) => rest = &rest[written..],
                Ok(Err(source)) => {
                    return Err(PtyError::Terminal {
                        what: "written to",
                        source,
                    })
                }
                // The descriptor was not writable after all; wait again.
                Err(_would_block) => continue,
            }
        }
        Ok(())
    }

    /// Tell the terminal it is a different size, and let the program on it
    /// know the way a terminal emulator does.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyError> {
        set_size(self.master.get_ref().as_raw_fd(), rows, cols)?;
        // The kernel raises SIGWINCH on the foreground group for us; a program
        // that redraws on resize is already listening for it.
        Ok(())
    }

    /// The size the terminal currently reports.
    pub fn size(&self) -> Result<(u16, u16), PtyError> {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        // Safety: the ioctl fills a `winsize` this call owns.
        let asked = unsafe {
            libc::ioctl(
                self.master.get_ref().as_raw_fd(),
                libc::TIOCGWINSZ,
                &mut size,
            )
        };
        if asked < 0 {
            return Err(PtyError::Terminal {
                what: "measured",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok((size.ws_row, size.ws_col))
    }

    /// Which process group currently owns the terminal.
    ///
    /// This is the group a `^C` would reach, and it is not the session leader
    /// whenever the leader is a shell running something: that is the whole
    /// point of asking rather than assuming.
    pub fn foreground_group(&self) -> Result<i32, PtyError> {
        // Safety: a plain query on a terminal descriptor this session owns.
        let group = unsafe { libc::tcgetpgrp(self.master.get_ref().as_raw_fd()) };
        if group <= 0 {
            return Err(PtyError::NoForeground);
        }
        Ok(group)
    }

    /// Deliver a signal to whichever group owns the terminal now.
    ///
    /// Answers the group that received it, because "it was delivered" and "it
    /// was delivered to the command rather than to the shell" are different
    /// facts and a caller reporting one should not have to guess the other.
    pub fn signal_foreground(&self, signal: i32) -> Result<i32, PtyError> {
        let group = self.foreground_group()?;
        // Safety: a plain `killpg` on a group this terminal reported.
        if unsafe { libc::killpg(group, signal) } != 0 {
            return Err(PtyError::Terminal {
                what: "signalled",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(group)
    }

    /// How the session ended, if it has.
    pub fn exit(&self) -> Option<PtyExit> {
        *self.exited.borrow()
    }

    /// Wait for the session to end *and* for everything it printed to be
    /// collected.
    ///
    /// The second half is not a nicety. A process exiting and its output
    /// arriving are two events, and the process wins the race routinely: a
    /// caller that waited only for the exit would read a transcript missing
    /// the last thing the program said, intermittently, on a loaded machine.
    /// The read loop ends when the terminal closes, which is after the last
    /// byte, so waiting for it is waiting for the whole story.
    pub async fn wait(&self) -> PtyExit {
        let exit = self.wait_for_exit().await;
        let mut drained = self.drained.clone();
        while !*drained.borrow() {
            if drained.changed().await.is_err() {
                break;
            }
        }
        exit
    }

    /// Wait only for the process, for a caller that is about to go on reading.
    pub async fn wait_for_exit(&self) -> PtyExit {
        let mut exited = self.exited.clone();
        loop {
            if let Some(exit) = *exited.borrow() {
                return exit;
            }
            if exited.changed().await.is_err() {
                return PtyExit {
                    code: None,
                    signal: None,
                };
            }
        }
    }

    /// End the session and everything in it, and wait until it is gone.
    ///
    /// Idempotent, and scoped to the terminal's whole *session* rather than to
    /// one process group. The difference is job control: an interactive shell
    /// puts each job it starts in a process group of its own, so `sleep 60 &`
    /// is outside the leader's group and a group kill cannot reach it - which
    /// is exactly how a harness that has exited leaves a `sleep` behind for an
    /// hour. Everything the terminal ever started shares the session the
    /// leader made with `setsid`, so that is the boundary swept.
    pub async fn close(&self) {
        self.closing.store(true, Ordering::Release);
        let mut exited = self.exited.clone();
        crate::proc::terminate_group(Some(self.leader as u32), self.grace, async move {
            while exited.borrow().is_none() {
                if exited.changed().await.is_err() {
                    return;
                }
            }
        })
        .await;
        self.sweep_session().await;
    }

    /// End whatever else is still on this terminal's session.
    ///
    /// A polite rung first, because a job that traps `SIGTERM` to tidy up
    /// deserves the chance it would get from a terminal hanging up; then
    /// `SIGKILL` for whatever is still there, because a session nobody owns
    /// any more must not outlive the harness that made it.
    async fn sweep_session(&self) {
        if in_session(self.leader).is_empty() {
            return;
        }
        signal_session(self.leader, libc::SIGHUP);
        signal_session(self.leader, libc::SIGTERM);
        let deadline = tokio::time::Instant::now() + self.grace;
        while tokio::time::Instant::now() < deadline {
            if in_session(self.leader).is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        signal_session(self.leader, libc::SIGKILL);
    }
}

/// Every process still in the terminal's session, this one aside.
///
/// Asked of `/proc` because the kernel offers no "signal this session" call:
/// `killpg` reaches one process group, and job control is precisely the
/// business of putting things in other ones. The session id outlives its
/// leader, so this still finds a job that was orphaned when the shell died.
#[cfg(target_os = "linux")]
fn in_session(session: i32) -> Vec<i32> {
    let mut found = Vec::new();
    let ours = std::process::id() as i32;
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if pid == ours {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `comm` is in parentheses and may hold spaces and parentheses of its
        // own, so the fields are counted from after the last `)`: state, ppid,
        // pgrp, session.
        let Some(after) = stat.rfind(')').map(|at| &stat[at + 1..]) else {
            continue;
        };
        let mut fields = after.split_whitespace();
        if let (Some(_state), Some(_ppid), Some(_pgrp), Some(sid)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next().and_then(|sid| sid.parse::<i32>().ok()),
        ) {
            if sid == session {
                found.push(pid);
            }
        }
    }
    found
}

/// Deliver one signal to everything left on a terminal's session.
#[cfg(target_os = "linux")]
fn signal_session(session: i32, signal: i32) {
    for pid in in_session(session) {
        // Safety: a plain `kill` on a pid this process just read out of
        // `/proc`. A pid that has exited in between answers `ESRCH`, which is
        // the answer this loop wants anyway.
        unsafe { libc::kill(pid, signal) };
    }
}

/// The master side of a pty, owned so it closes with the session.
struct Master(OwnedFd);

impl AsRawFd for Master {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// Open a terminal pair and answer the master plus the slave's path.
fn allocate() -> Result<(Master, PathBuf), PtyError> {
    // Safety: the three calls below are the POSIX allocation sequence, each
    // checked; `posix_openpt` returns a fresh descriptor this call owns.
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
    if master < 0 {
        return Err(PtyError::Allocate(std::io::Error::last_os_error()));
    }
    let master = Master(unsafe { OwnedFd::from_raw_fd(master) });
    if unsafe { libc::grantpt(master.as_raw_fd()) } < 0 {
        return Err(PtyError::Allocate(std::io::Error::last_os_error()));
    }
    if unsafe { libc::unlockpt(master.as_raw_fd()) } < 0 {
        return Err(PtyError::Allocate(std::io::Error::last_os_error()));
    }

    let mut name = [0 as libc::c_char; 128];
    // Safety: `ptsname_r` fills a buffer this call owns, with its length.
    if unsafe { libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) } != 0 {
        return Err(PtyError::Allocate(std::io::Error::last_os_error()));
    }
    // Safety: the kernel wrote a null-terminated path into this buffer, and
    // `CStr` borrows it rather than taking ownership - the buffer is on this
    // stack and freeing it would be freeing memory nothing allocated.
    let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) };
    let path = PathBuf::from(OsStr::from_bytes(path.to_bytes()));

    // The master is read by an async task, so it must never block the runtime.
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(PtyError::Allocate(std::io::Error::last_os_error()));
    }
    Ok((master, path))
}

/// Set the terminal's size.
fn set_size(fd: RawFd, rows: u16, cols: u16) -> Result<(), PtyError> {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        // Pixel dimensions are what a graphical terminal reports; a program
        // reading them from us gets zero, which is what "not a screen" means.
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // Safety: the ioctl reads a `winsize` this call owns.
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &size) } < 0 {
        return Err(PtyError::Terminal {
            what: "resized",
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(())
}

/// Write once, reporting a would-block to the caller's retry loop.
fn write_fd(fd: RawFd, data: &[u8]) -> std::io::Result<usize> {
    // Safety: writes `data.len()` bytes from a slice that outlives the call.
    let written = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
    if written < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(written as usize)
}

/// Read the terminal until it ends, into the transcript.
///
/// The loop never waits for a consumer: everything the child writes is taken
/// out of the kernel's buffer as soon as it is there, which is what keeps a
/// fast writer from blocking and a slow reader from losing anything the bound
/// would otherwise have room for.
async fn drain(master: Arc<AsyncFd<Master>>, transcript: Arc<Transcript>) {
    let mut chunk = [0u8; 65536];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let Ok(mut guard) = master.readable().await else {
            return;
        };
        let read = match guard.try_io(|inner| read_fd(inner.get_ref().as_raw_fd(), &mut chunk)) {
            Ok(Ok(0)) => return,
            Ok(Ok(read)) => read,
            // A pty master reports `EIO` when the last slave closes, which is
            // this side's end-of-file rather than a fault.
            Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => return,
            Ok(Err(_)) => return,
            Err(_would_block) => continue,
        };

        // A terminal is a byte stream and a multi-byte character can be split
        // across two reads, so a partial one waits here for the rest instead of
        // reaching the transcript as a glyph nothing printed.
        pending.extend_from_slice(&chunk[..read]);
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(error) => error.valid_up_to(),
        };
        if valid > 0 {
            let rest = pending.split_off(valid);
            let text =
                String::from_utf8(std::mem::replace(&mut pending, rest)).expect("valid up to here");
            transcript.push(&text);
        }
    }
}

/// Read once, reporting a would-block to the caller's retry loop.
fn read_fd(fd: RawFd, buffer: &mut [u8]) -> std::io::Result<usize> {
    // Safety: reads at most `buffer.len()` bytes into a slice this call owns.
    let read = unsafe { libc::read(fd, buffer.as_mut_ptr() as *mut libc::c_void, buffer.len()) };
    if read < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(read as usize)
}

/// Wait for the session leader and classify how it ended.
fn wait_for(mut child: std::process::Child) -> PtyExit {
    use std::os::unix::process::ExitStatusExt;
    match child.wait() {
        Ok(status) => PtyExit {
            code: status.code(),
            signal: status.signal(),
        },
        Err(_) => PtyExit {
            code: None,
            signal: None,
        },
    }
}
