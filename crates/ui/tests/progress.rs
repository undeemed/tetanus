//! Test Design Specification: the status line.
//!
//! Features tested: what an animated line writes and re-writes, that a plain
//! stream gets whole lines and no frames, that a repeated phase is silent,
//! that an over-long label is cut instead of wrapping, that clearing erases
//! exactly what was drawn - by the columns drawn and not the characters
//! written - and that the charset chooses the glyphs.
//!
//! Features NOT tested here: terminal detection (owned by `Policy`, and a
//! one-line call to `IsTerminal`) and the colour rules (owned by
//! `color_policy.rs`).
//!
//! Environmental needs: none. Every case writes into a `Vec<u8>`, so no case
//! needs a terminal, a pty, or a variable set on the process.

use tetanus_ui::{buffered, Charset, Progress, Theme};

fn animated(charset: Charset, width: usize) -> Progress<Vec<u8>> {
    Progress::new(buffered(Theme::new(false, charset), width), true)
}

fn plain(width: usize) -> Progress<Vec<u8>> {
    Progress::new(buffered(Theme::new(false, Charset::Unicode), width), false)
}

fn written(progress: Progress<Vec<u8>>) -> String {
    progress.ui().contents()
}

/// TC-UI-PROG-1: the first frame at a terminal.
/// Expected: a carriage return, the first spinner glyph, and the label. No
/// newline, because the line is about to be repainted.
#[test]
fn the_first_frame_opens_the_line() {
    let mut progress = animated(Charset::Unicode, 40);
    progress.set("calling deepseek-v4-flash").expect("write");

    assert_eq!(written(progress), "\r⠋ calling deepseek-v4-flash");
}

/// TC-UI-PROG-2: a tick with the phase unchanged.
/// Expected: the previous line is erased with spaces and redrawn one glyph on.
/// Erasing uses spaces and not `ESC[2K`, so a plain theme emits no escapes.
#[test]
fn a_tick_repaints_the_same_line() {
    let mut progress = animated(Charset::Unicode, 40);
    progress.set("thinking").expect("write");
    progress.tick().expect("write");

    let out = written(progress);
    assert_eq!(out, "\r⠋ thinking\r          \r⠙ thinking");
    assert!(
        !out.contains('\u{1b}'),
        "escape codes in a plain theme: {out:?}"
    );
    assert!(!out.contains('\n'), "the status line scrolled: {out:?}");
}

/// TC-UI-PROG-3: the same label announced twice, on a pipe.
/// Expected: one line. A caller may call `set` in a loop; only a real phase
/// change reaches the log.
#[test]
fn a_pipe_gets_phases_not_frames() {
    let mut progress = plain(40);
    progress.set("running tools").expect("write");
    progress.tick().expect("write");
    progress.set("running tools").expect("write");
    progress.set("writing the answer").expect("write");

    assert_eq!(written(progress), "running tools\nwriting the answer\n");
}

/// TC-UI-PROG-4: a label wider than the line.
/// Expected: cut to exactly the width, ending in an ellipsis. A status line
/// that wraps scrolls the terminal, and the next repaint lands on the wrong
/// row.
#[test]
fn an_over_long_label_is_cut_to_the_width() {
    let mut progress = animated(Charset::Unicode, 20);
    progress
        .set("calling a model with a very long name")
        .expect("write");

    let out = written(progress);
    let drawn = out.trim_start_matches('\r');
    assert_eq!(drawn.chars().count(), 20, "{drawn:?}");
    assert!(drawn.ends_with('…'), "{drawn:?}");
}

/// TC-UI-PROG-5: clearing after a wide frame then a narrow one.
/// Expected: the erase covers the frame actually on screen, and the cursor is
/// returned to column 0 so the caller's next line starts clean.
#[test]
fn clearing_erases_exactly_what_was_drawn() {
    let mut progress = animated(Charset::Unicode, 40);
    progress.set("a long phase name").expect("write");
    progress.set("short").expect("write");
    progress.clear().expect("write");

    let out = written(progress);
    assert!(out.ends_with("\r⠋ short\r       \r"), "{out:?}");

    let mut empty = animated(Charset::Unicode, 40);
    empty.clear().expect("write");
    assert_eq!(written(empty), "", "clearing an unopened line wrote bytes");
}

/// TC-UI-PROG-6: an ASCII charset.
/// Expected: ASCII frames and an ASCII ellipsis, so a terminal that cannot
/// render braille shows a spinner rather than replacement characters.
#[test]
fn an_ascii_terminal_gets_ascii_frames() {
    let mut progress = animated(Charset::Ascii, 12);
    progress.set("connecting to the provider").expect("write");
    progress.tick().expect("write");

    let out = written(progress);
    // Twelve columns: the glyph, a space, and nine columns of label.
    assert!(out.starts_with("\r- connect..."), "{out:?}");
    assert!(out.ends_with("\r\\ connect..."), "{out:?}");
    assert!(out.is_ascii(), "{out:?}");
}

/// TC-UI-PROG-7: colour reaches the status line.
/// Expected: the frame is styled when the theme says so, and `finish` hands
/// back a usable `Ui` with the line erased.
#[test]
fn a_colour_theme_styles_the_frame_and_finish_returns_the_stream() {
    let ui = buffered(Theme::new(true, Charset::Unicode), 40);
    let mut progress = Progress::new(ui, true);
    progress.set("thinking").expect("write");

    let mut ui = progress.finish().expect("write");
    ui.line("done").expect("write");

    let out = ui.contents();
    assert!(out.contains('\u{1b}'), "the frame was not styled: {out:?}");
    assert!(out.ends_with("\r          \rdone\n"), "{out:?}");
}

/// TC-UI-PROG-8: a label the terminal draws two columns per character.
/// Expected: the erase is as many spaces as the frame drew columns - eighteen
/// here, not the fourteen characters it was written with. A status line that
/// erases by characters leaves the tail of a wide label on the row, under
/// whatever the caller writes next.
#[test]
fn a_wide_label_is_erased_by_the_columns_it_drew() {
    let mut progress = animated(Charset::Unicode, 40);
    progress.set("calling 模型模型").expect("write");
    progress.clear().expect("write");

    // The glyph, a space, eight ASCII columns, and four characters of two.
    let out = written(progress);
    assert!(out.ends_with(&format!("\r{}\r", " ".repeat(18))), "{out:?}");
}
