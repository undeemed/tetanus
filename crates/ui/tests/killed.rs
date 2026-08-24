//! Test Design Specification: the terminal is given back when the process is
//! killed rather than ended.
//!
//! Features tested: that a signal which ends a process runs the give-back
//! first, and that the process still reports itself killed by that signal
//! afterwards. Features NOT tested here: what the give-back writes. That is
//! [`Tty`](tetanus_ui::Tty)'s two calls into `crossterm` against a real
//! controlling terminal, which a `cargo test` process does not have, so the
//! console here records instead of drawing. A real terminal, entered and left
//! under all four signals, is covered end to end by `target/probe-sig.py`.
//!
//! Approach: a case cannot assert this in its own process, because the case
//! would be the process that died. It runs the test binary again as a child,
//! which holds a recording console and waits, kills the child, and reads back
//! what the child managed to say and what killed it.
//!
//! Environmental needs: a unix host, for signals and for `kill`. On any other
//! target the whole file compiles to nothing, the same way the watch does.
//!
//! Signals NOT driven here: `SIGQUIT`. Its default action writes a core file
//! into whatever directory the suite is run from, and a test that litters is
//! a test people turn off. `probe-sig.py` drives it.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// Set on the child, and only on the child.
const CHILD: &str = "TETANUS_UI_KILLED_CHILD";

/// The name the child announces itself with, once the watch is up.
const ARMED: &str = "armed";

/// What the child's console records when the terminal is given back.
const GIVEN: &str = "given back";

/// A console that says what was asked of it instead of doing it.
struct Recording;

impl tetanus_ui::Console for Recording {
    fn take(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn restore(&mut self) -> std::io::Result<()> {
        // `println!` flushes on the newline, so this has reached the pipe
        // before the handler goes on to end the process.
        println!("{GIVEN}");
        Ok(())
    }
}

/// The other half of every case below: a process that holds a terminal.
///
/// A `#[test]` because that is the only entry point a test binary has. Run by
/// the suite it does nothing; run by a case here, with [`CHILD`] set, it hangs
/// the watch, says so, and waits to be killed.
#[test]
fn child_holds_a_terminal_until_it_is_killed() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let _killed = tetanus_ui::when_killed(Recording).expect("hang the watch");
    println!("{ARMED}");
    // Longer than any case needs, so a child that is somehow not killed fails
    // the case that spawned it rather than hanging the suite for ever.
    std::thread::sleep(std::time::Duration::from_secs(30));
}

/// Start the child and wait until its watch is up.
fn armed_child() -> std::process::Child {
    let mut child = Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "child_holds_a_terminal_until_it_is_killed",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the child");

    let out = child.stdout.as_mut().expect("piped");
    let mut lines = BufReader::new(out).lines();
    // `ends_with`, not equality: with `--nocapture` under one test thread,
    // libtest writes `test <name> ... ` with no newline before the test runs,
    // so the child's marker lands on the end of that line rather than on one
    // of its own. At more than one thread the header is written afterwards and
    // the marker is alone. Matching the end of the line reads both.
    let armed = lines
        .by_ref()
        .any(|line| line.expect("read the child").ends_with(ARMED));
    if armed {
        return child;
    }
    // Reaped before the panic, so a child that started but never armed is not
    // left behind holding a terminal that nothing will now give back.
    child.kill().ok();
    child.wait().ok();
    panic!("the child ended before it armed the watch");
}

/// TC-UI-TERM-5: a signal that ends the process gives the terminal back first,
/// and the process still dies of that signal.
///
/// Expected, for each of `SIGTERM`, `SIGHUP` and `SIGINT`: the child's console
/// recorded exactly one give-back, and the child's status names that signal.
///
/// Without the watch the child records nothing at all - `Drop` does not run on
/// a signal - which is a person left in raw mode on the alternate screen with
/// no cursor, and no way out but to type `reset` blind.
#[test]
fn a_killing_signal_gives_the_terminal_back() {
    use std::os::unix::process::ExitStatusExt;

    for (name, number) in [("TERM", 15), ("HUP", 1), ("INT", 2)] {
        let child = armed_child();
        let killed = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(child.id().to_string())
            .status()
            .expect("kill the child");
        assert!(killed.success(), "SIG{name}: kill said {killed}");

        let out = child.wait_with_output().expect("wait for the child");
        let said = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            said.lines().filter(|line| *line == GIVEN).count(),
            1,
            "SIG{name}: the terminal was given back {} times: {said:?}",
            said.lines().filter(|line| *line == GIVEN).count()
        );
        assert_eq!(
            out.status.signal(),
            Some(number),
            "SIG{name}: the child reported {:?} instead",
            out.status
        );
    }
}
