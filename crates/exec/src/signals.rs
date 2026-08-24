//! The signal disposition a child is given, and why it has to be given one.
//!
//! **A signal set to `SIG_IGN` is inherited across `fork` *and* across
//! `exec`.** So is a blocked signal mask. That is the mechanism behind a bug
//! that reads as flakiness and is not: everything this harness starts inherits
//! whatever the harness was started with, and *what the harness was started
//! with depends on who started it*.
//!
//! The case that matters is ordinary. POSIX (2.11, "Signals and Error
//! Handling") says a shell running a command asynchronously - `tetanus serve
//! &`, or any orchestrator that does the same thing - sets `SIGINT` and
//! `SIGQUIT` to `SIG_IGN` in the child, because a background job must not die
//! when somebody presses `^C` at the terminal it was launched from. tetanus
//! then starts a shell on a pseudo-terminal, that shell inherits the ignore,
//! every command it runs inherits it in turn, and:
//!
//! - `terminal_signal` reports `delivered SIGINT to foreground process group
//!   N` and the command does not stop;
//! - the turn's interrupt reaches the right group and the work continues;
//! - `killpg` returns success throughout, because delivery *did* succeed. The
//!   process simply ignores it.
//!
//! Measured before it was fixed: the same code, the same machine, no load -
//! run from an interactive shell a `sleep 30` on a terminal dies on `SIGINT`,
//! and run with `&` it survives, with the harness reporting success both
//! times. It looked like a load-dependent race for exactly as long as nobody
//! compared those two runs, because a busy machine is also a machine somebody
//! is driving from a script rather than by hand.
//!
//! So every child this crate starts is given the disposition a program started
//! from a terminal would have: the ignorable signals back to `SIG_DFL`, and an
//! empty signal mask. `bash`, `sudo` and `tmux` all do the same thing at the
//! same point, and for the same reason.

/// The signals reset to their default disposition in a child.
///
/// The ones a parent plausibly ignores or blocks and a child must not inherit:
/// the two POSIX names a background launch ignores, the terminal-generated
/// stops, the two hangup and termination signals a supervisor may hold, and
/// `SIGPIPE`, which a Rust parent ignores process-wide so that a write to a
/// closed pipe is an error rather than a death - correct for this process, and
/// wrong for a shell, which is how `yes | head` learns to stop.
#[cfg(unix)]
const RESET: [libc::c_int; 8] = [
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGTERM,
    libc::SIGHUP,
    libc::SIGPIPE,
    libc::SIGTSTP,
    libc::SIGTTIN,
    libc::SIGTTOU,
];

/// Give this process the signal disposition a child should start with.
///
/// **Only ever called between `fork` and `exec`**, where the rules are narrow:
/// no allocation, no locks, no library code that might take one. Everything
/// here is a system call - `sigaction` through `signal`, and `sigprocmask` -
/// which is what makes it safe in that window.
///
/// Failures are returned rather than swallowed: a `pre_exec` hook that returns
/// an error makes the spawn fail, and a child that could not be given a usable
/// signal disposition is a child whose interrupt would silently do nothing
/// later. Better to refuse to start it.
#[cfg(unix)]
pub fn reset_for_child() -> std::io::Result<()> {
    for signal in RESET {
        // Safety: `signal` is a system call; `SIG_DFL` is a constant. No
        // allocation, no locks.
        if unsafe { libc::signal(signal, libc::SIG_DFL) } == libc::SIG_ERR {
            return Err(std::io::Error::last_os_error());
        }
    }
    // A blocked signal is inherited across `exec` too, so a child of a process
    // that blocked `SIGINT` would be as deaf as one that ignored it.
    // Safety: both calls are system calls on a mask this frame owns.
    unsafe {
        let mut empty: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut empty) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn reset_for_child() -> std::io::Result<()> {
    Ok(())
}
