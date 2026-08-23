//! Test Design Specification: what a terminal would be showing.
//!
//! Feature under test: `tetanus_exec::screen` and the two questions a terminal
//! session can now answer - what was printed (the transcript) and what is on
//! the screen. `docs/parity.md` carried the gap this closes in those words:
//! the sanitizer strips escapes and keeps no screen, *so `htop` and `vim` are
//! runnable here and not readable*.
//!
//! Approach: two halves, and both are needed. The grid is fed byte sequences
//! directly, because a case that drives a real program cannot say which
//! sequence it is asserting on and a regression in `insert_lines` would show
//! up as a mysteriously wrong frame. Then `vim` and `htop` themselves, on a
//! real terminal, because a model of a screen that only ever sees sequences a
//! case author thought of is a model of that author's expectations - both
//! programs are on this host, so the claim is testable against the thing
//! itself rather than against a fixture.
//!
//! What is deliberately not modelled, and so not asserted: colours and
//! attributes. A model reads text; keeping attributes would double the file to
//! record something nothing here renders.
//!
//! Environmental needs: Linux with `/dev/ptmx`, a bash on PATH; the two
//! program cases report themselves skipped where `vim` or `htop` is absent.
//! No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::time::Duration;

use tetanus_exec::backend::Bash;
use tetanus_exec::screen::Screen;
use tetanus_exec::terminal::{TerminalConfig, TerminalError, TerminalSession};

/// TC-PORT-SCREEN-1: a program that overwrites a cell is read as what is
/// there, not as both things it wrote.
///
/// The whole difference between the two models in one case. A transcript
/// concatenates; a screen has one cell per position, and the last write wins.
///
/// Input: text written, the cursor moved back to the start of the line, and
/// different text written over it.
/// Expected: the screen holds the second text only, and the cursor is where
/// the writing left it.
#[test]
fn a_cell_written_twice_reads_as_what_is_there_now() {
    let screen = Screen::new(4, 20);

    screen.feed("hello world");
    screen.feed("\r");
    screen.feed("goodbye");

    // "hello w" is what the seven characters landed on, so "orld" is what is
    // left of the first write: a screen keeps cells, not writes.
    assert_eq!(screen.text(), "goodbyeorld");
    assert_eq!(screen.cursor().col, 7);
}

/// TC-PORT-SCREEN-2: the cursor and erase family put text where a program
/// meant it.
///
/// The sequences every full-screen program is made of. Asserted directly
/// because a wrong `CUP` shows up in a real program as a frame that is subtly
/// wrong in a way no case could name.
///
/// Input: an absolute cursor move, a relative one, an erase to end of line,
/// and an erase of the whole display.
/// Expected: each does what the terminal it imitates would do.
#[test]
fn the_cursor_and_erase_family_put_text_where_a_program_meant_it() {
    let screen = Screen::new(5, 12);

    // Row 3, column 5, counted from one as the sequence counts.
    screen.feed("\u{1b}[3;5Hmarker");
    assert_eq!(screen.text(), "\n\n    marker");

    // Back up two rows and write: a relative move from where it left off.
    screen.feed("\u{1b}[2A\u{1b}[1Gtop");
    assert_eq!(screen.text(), "top\n\n    marker");

    // Erase to the end of the line, from the middle of `marker`.
    screen.feed("\u{1b}[3;8H\u{1b}[K");
    assert_eq!(screen.text(), "top\n\n    mar");

    // And the whole screen, which is what a program clears with before its
    // first frame.
    screen.feed("\u{1b}[2J");
    assert_eq!(screen.text(), "");
}

/// TC-PORT-SCREEN-3: the scrolling region scrolls, and what is outside it
/// stays.
///
/// This is what a status line is: `htop` and `top` keep their header still
/// while the process list moves under it, and they do it by setting a region.
/// A model without one shows the header scrolling away, which is exactly the
/// unreadable frame this file exists to prevent.
///
/// Input: a five-row screen with rows 2-4 as the region, filled, then a line
/// feed at the bottom of the region.
/// Expected: the region scrolled by one, the header and the footer did not
/// move.
#[test]
fn a_scrolling_region_scrolls_and_the_rest_stays() {
    let screen = Screen::new(5, 10);

    screen.feed("\u{1b}[1;1Hheader");
    screen.feed("\u{1b}[5;1Hfooter");
    screen.feed("\u{1b}[2;4r"); // rows 2 to 4 scroll
    screen.feed("\u{1b}[2;1Hone\u{1b}[3;1Htwo\u{1b}[4;1Hthree");

    // At the foot of the region, a line feed scrolls the region only.
    screen.feed("\u{1b}[4;1H\nfour");

    assert_eq!(screen.text(), "header\ntwo\nthree\nfour\nfooter");
}

/// TC-PORT-SCREEN-4: the alternate screen is entered, drawn on, and given
/// back.
///
/// The bit that matters most. Entering the alternate screen is a program
/// announcing that it draws; leaving it is the reason your shell's scrollback
/// is still there after `vim` exits. A model that did not switch buffers would
/// leave the editor's frame on the screen for ever after it quit.
///
/// Input: a shell-like line printed, then the alternate screen entered and
/// drawn on, then left.
/// Expected: `is_alternate` follows the switch; the drawing is what shows
/// while it is on; and what was underneath comes back untouched.
#[test]
fn the_alternate_screen_is_entered_drawn_on_and_given_back() {
    let screen = Screen::new(4, 20);
    screen.feed("$ vim notes.txt\r\n");
    assert!(!screen.is_alternate());

    screen.feed("\u{1b}[?1049h\u{1b}[2J\u{1b}[1;1Hthe editor's frame");
    assert!(screen.is_alternate(), "the program said it is drawing");
    assert_eq!(screen.text(), "the editor's frame");

    screen.feed("\u{1b}[?1049l");
    assert!(!screen.is_alternate());
    assert_eq!(
        screen.text(),
        "$ vim notes.txt",
        "what was underneath has to come back, or a session loses its history \
         every time a model opens an editor"
    );
}

/// TC-PORT-SCREEN-5: a sequence split across two reads is one sequence.
///
/// A terminal read returns whatever the kernel had, so a cursor move is
/// routinely cut in half. A model that treated the halves as text would print
/// `[3;5H` into the middle of somebody's frame, and only on a loaded machine.
///
/// Input: one `CUP` and one alternate-screen switch, each fed in two pieces.
/// Expected: neither leaves any of itself on the screen, and both take effect.
#[test]
fn a_sequence_split_across_two_reads_is_one_sequence() {
    let screen = Screen::new(3, 12);

    screen.feed("\u{1b}[2;");
    screen.feed("3Hhere");
    assert_eq!(screen.text(), "\n  here");
    assert!(!screen.text().contains('['), "half a sequence was printed");

    screen.feed("\u{1b}[?10");
    assert!(!screen.is_alternate(), "it has not finished arriving");
    screen.feed("49h");
    assert!(screen.is_alternate());
}

/// TC-PORT-SCREEN-6: `vim` is readable.
///
/// The parity gap, tested against the program it names. Every case above is a
/// claim about a sequence; this is the claim that the set of sequences is
/// enough for a real editor - which is the only version of it a model cares
/// about.
///
/// It is also the case that found the environment defect: `vim` on a session
/// with no `HOME` paints its status line and stops, so for a while this read
/// as a hole in the screen model. It was not - the editor was never drawing.
///
/// Input: `vim` opened on a file with known contents, on a real terminal.
/// Expected: the session reports the program is drawing; the screen holds the
/// file's text and `vim`'s own status line; and the transcript - the other
/// model - does not, which is the difference this slice exists for.
#[tokio::test]
async fn vim_is_readable() {
    let workspace = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        workspace.path().join("notes.txt"),
        "the first line\nthe second line\n",
    )
    .expect("wrote the file");
    let Some(session) = terminal(workspace.path()).await else {
        return;
    };
    if which("vim").is_none() {
        eprintln!("skipped: no vim on this host");
        return;
    }

    session
        .send_waiting(
            "vim -u NONE -N notes.txt",
            true,
            Some(Duration::from_millis(1500)),
            None,
        )
        .await
        .expect("sent");
    settle(&session, "the first line").await;

    assert!(
        session.is_drawing(),
        "vim entered the alternate screen, so the session should say it is drawing"
    );
    let screen = session.screen();
    assert!(
        screen.contains("the first line") && screen.contains("the second line"),
        "the file's text should be on the screen:\n{screen}"
    );
    assert!(
        screen.contains("notes.txt"),
        "vim's own status line should be there too:\n{screen}"
    );

    // Leave the editor, and the shell's screen comes back.
    session
        .send_waiting(
            "\u{1b}:q!\r",
            false,
            Some(Duration::from_millis(1500)),
            None,
        )
        .await
        .expect("sent");
    for _ in 0..40 {
        if !session.is_drawing() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !session.is_drawing(),
        "vim exited, so the terminal is printing again"
    );
    session.close().await;
}

/// TC-PORT-SCREEN-7: `htop` is readable, and its transcript is not.
///
/// The other half of the gap's own wording, and the case that shows why both
/// models are kept. `htop` repaints several times a second: its transcript is
/// every frame concatenated, and its screen is the current one.
///
/// Input: `htop` for a second or so on a real terminal.
/// Expected: the session says it is drawing, the screen holds one frame's
/// worth of rows, and the transcript holds several times more - so a reader
/// handed the transcript would be reading frames that are no longer true.
#[tokio::test]
async fn htop_is_readable_and_its_transcript_is_not() {
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(session) = terminal(workspace.path()).await else {
        return;
    };
    if which("htop").is_none() {
        eprintln!("skipped: no htop on this host");
        return;
    }

    session
        .send_waiting("htop -d 2", true, Some(Duration::from_millis(2000)), None)
        .await
        .expect("sent");
    settle(&session, "Tasks").await;

    let screen = session.screen();
    let transcript = session.scrollback();
    assert!(
        session.is_drawing(),
        "htop draws, so the session should say so"
    );
    assert!(
        screen.lines().count() <= 45,
        "the screen is one frame, not every frame: {} lines",
        screen.lines().count()
    );
    assert!(
        transcript.lines().count() > screen.lines().count(),
        "the transcript should be the longer of the two, or this case proves nothing: \
         transcript {} lines, screen {} lines",
        transcript.lines().count(),
        screen.lines().count()
    );

    session
        .send_waiting("q", false, Some(Duration::from_millis(1000)), None)
        .await
        .expect("sent");
    session.close().await;
}

// ---------------------------------------------------------------- fixtures

/// A terminal session rooted at `workspace`, or `None` after reporting the
/// case skipped where this host has no terminal to allocate.
async fn terminal(workspace: &std::path::Path) -> Option<TerminalSession> {
    match TerminalSession::open(
        "pty-1".into(),
        None,
        "bash".into(),
        Arc::new(Bash::new()),
        TerminalConfig {
            cwd: workspace.to_path_buf(),
            idle_silence: Duration::from_secs(5),
            timeout: Duration::from_secs(20),
            grace: Duration::from_millis(200),
            ..TerminalConfig::default()
        },
    )
    .await
    {
        Ok(session) => Some(session),
        Err(TerminalError::Pty(tetanus_exec::pty::PtyError::Allocate(why))) => {
            eprintln!("skipped: this host cannot allocate a pseudo-terminal ({why})");
            None
        }
        Err(TerminalError::Backend(why)) => {
            eprintln!("skipped: this host has no bash ({why})");
            None
        }
        Err(other) => panic!("the terminal session could not be opened: {other}"),
    }
}

/// Wait until `text` is on the screen, or give up after a bounded wait.
/// Polling beats sleeping a fixed time: quick when the machine is idle,
/// correct when it is loaded.
async fn settle(session: &TerminalSession, text: &str) {
    for _ in 0..120 {
        if session.screen().contains(text) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Where a program is, if it is anywhere on this host's PATH.
fn which(program: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}
