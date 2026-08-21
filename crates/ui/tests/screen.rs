//! Test Design Specification: the redrawable block.
//!
//! Features tested: that a plain stream never sees a frame or an escape, what
//! the first frame writes, how a second frame returns to the block's top, that
//! a shorter frame erases what the taller one left, that a printed line leaves
//! the block for good, that a painted line is cut by what it draws, and what a
//! resize does to the block already on screen.
//!
//! Features NOT tested here: terminal detection (owned by `Policy`), the
//! colour rules (owned by `color_policy.rs`), and what any particular view
//! puts in the block.
//!
//! Environmental needs: none. Every case writes into a `Vec<u8>`, so no case
//! needs a terminal or a pty.
//!
//! Features tested here also include the height: a block taller than the
//! terminal keeps its tail, and a terminal made shorter under a block holds
//! the next frame to the new height.

use tetanus_ui::{buffered, visible_width, Charset, Screen, Theme};

fn screen(animated: bool, width: usize) -> Screen<Vec<u8>> {
    // A terminal tall enough that the cases below say what they mean about
    // width and about erasing; the height has its own case.
    tall(animated, width, 24)
}

fn tall(animated: bool, width: usize, rows: usize) -> Screen<Vec<u8>> {
    Screen::new(
        buffered(Theme::new(false, Charset::Unicode), width),
        animated,
        rows,
    )
}

fn lines(text: &[&str]) -> Vec<String> {
    text.iter().map(|line| line.to_string()).collect()
}

fn written(screen: Screen<Vec<u8>>) -> String {
    screen.ui().contents()
}

/// TC-UI-SCREEN-1: a piped stream.
/// Expected: the frames write nothing at all, the printed lines are the whole
/// output, and no escape code reaches the pipe. This is the case that keeps a
/// piped run byte-identical to a run with no terminal attached.
#[test]
fn a_pipe_gets_the_printed_lines_and_no_frames() {
    let mut screen = screen(false, 40);
    screen.draw(&lines(&["working", "  echo"])).expect("frame");
    screen.print(&lines(&["you  hello"])).expect("print");
    screen.draw(&lines(&["still working"])).expect("frame");

    let out = written(screen);
    assert_eq!(out, "you  hello\n");
    assert!(!out.contains('\u{1b}'), "{out:?}");
}

/// TC-UI-SCREEN-2: the first frame at a terminal.
/// Expected: no cursor move, because nothing is drawn yet; one row per line,
/// each closed by an erase so a longer row underneath cannot show through.
#[test]
fn the_first_frame_moves_no_cursor() {
    let mut screen = screen(true, 40);
    screen
        .draw(&lines(&["ai  on it", "tool  echo"]))
        .expect("frame");

    assert_eq!(written(screen), "\rai  on it\u{1b}[K\ntool  echo\u{1b}[K\n");
}

/// TC-UI-SCREEN-3: the frame after it.
/// Expected: the cursor goes up by exactly the rows the last frame drew, and
/// the block is written again from there. Off by one here and the view eats
/// the line above it, which is the failure this case exists to catch.
#[test]
fn the_next_frame_returns_to_the_top_of_the_block() {
    let mut screen = screen(true, 40);
    screen.draw(&lines(&["one", "two"])).expect("frame");
    let first = screen.ui().contents().len();
    screen.draw(&lines(&["one", "two!"])).expect("frame");

    let out = written(screen);
    assert_eq!(&out[first..], "\u{1b}[2A\rone\u{1b}[K\ntwo!\u{1b}[K\n");
}

/// TC-UI-SCREEN-4: a frame shorter than the one before it.
/// Expected: the rows the taller frame left are erased, and the cursor comes
/// back to the row under the new block. A view whose tool list shrinks would
/// otherwise leave the last tool on screen for ever.
#[test]
fn a_shorter_frame_erases_what_is_left_over() {
    let mut screen = screen(true, 40);
    screen
        .draw(&lines(&["one", "two", "three"]))
        .expect("frame");
    let first = screen.ui().contents().len();
    screen.draw(&lines(&["one"])).expect("frame");

    let out = written(screen);
    assert_eq!(
        &out[first..],
        "\u{1b}[3A\rone\u{1b}[K\n\u{1b}[K\n\u{1b}[K\n\u{1b}[2A"
    );
}

/// TC-UI-SCREEN-5: a line committed while a block is on screen.
/// Expected: the block is erased first, so the committed line lands where the
/// block was and the next frame draws under it. Printing without erasing
/// leaves a copy of the block above every committed line.
#[test]
fn a_printed_line_takes_the_blocks_place() {
    let mut screen = screen(true, 40);
    screen.draw(&lines(&["working"])).expect("frame");
    let first = screen.ui().contents().len();
    screen.print(&lines(&["ai  done"])).expect("print");

    let out = written(screen);
    assert_eq!(&out[first..], "\u{1b}[1A\r\u{1b}[Jai  done\n");
}

/// TC-UI-SCREEN-6: a painted line wider than the terminal.
/// Expected: it is cut to the width it draws, not the characters it holds, so
/// one line stays one row. A wrapped row puts every later frame out by one.
#[test]
fn a_painted_line_is_cut_to_one_row() {
    let mut screen = screen(true, 12);
    let painted = format!("\u{1b}[1m{}\u{1b}[0m", "x".repeat(40));
    screen.draw(&[painted]).expect("frame");

    let out = written(screen);
    let row = out
        .trim_start_matches('\r')
        .trim_end_matches("\u{1b}[K\n")
        .to_string();
    assert_eq!(visible_width(&row), 12, "{row:?}");
    assert_eq!(out.matches('\n').count(), 1, "{out:?}");
}

/// TC-UI-SCREEN-7: handing the stream back.
/// Expected: the block is erased, so the caller's own last word starts on the
/// row the block began on and nothing of the live view survives it.
#[test]
fn finishing_erases_the_block() {
    let mut screen = screen(true, 40);
    screen.draw(&lines(&["working", "  echo"])).expect("frame");
    let first = screen.ui().contents().len();
    let ui = screen.finish().expect("finish");

    assert_eq!(&ui.contents()[first..], "\u{1b}[2A\r\u{1b}[J");
}

/// TC-UI-SCREEN-8: the terminal is resized under a block.
/// Expected: the block on screen is erased, because it was fitted to a width
/// that is no longer the terminal's, and the frame after it is cut to the new
/// one. A block kept at the old width wraps in a narrower window, and a
/// wrapped row puts every later frame out of place.
#[test]
fn a_resize_erases_the_block_the_old_width_drew() {
    let mut screen = screen(true, 40);
    screen
        .draw(&lines(&["ai  on it", "tool  echo"]))
        .expect("frame");
    let drawn = screen.ui().contents().len();

    screen.resize(12).expect("resize");
    screen.draw(&["x".repeat(40)]).expect("frame");

    let out = written(screen);
    let after = &out[drawn..];
    assert!(
        after.starts_with("\u{1b}[2A\r\u{1b}[J"),
        "the old block was left drawn: {after:?}"
    );
    let row = after
        .trim_start_matches("\u{1b}[2A\r\u{1b}[J")
        .trim_start_matches('\r')
        .trim_end_matches("\u{1b}[K\n");
    assert_eq!(visible_width(row), 12, "{row:?}");
}

/// TC-UI-SCREEN-9: a resize to the width already in force.
/// Expected: nothing is written. The width is asked for on every frame, so the
/// answer is the same one twelve times a second; a redraw each time would make
/// a still block flicker for no reason.
#[test]
fn a_resize_to_the_same_width_writes_nothing() {
    let mut screen = screen(true, 40);
    screen.draw(&lines(&["ai  on it"])).expect("frame");
    let drawn = screen.ui().contents().len();

    screen.resize(40).expect("resize");

    assert_eq!(written(screen).len(), drawn);
}

/// TC-UI-SCREEN-8: a block taller than the terminal.
/// Expected: the last rows of it, one row short of the terminal's height, and
/// the rows above them dropped. A block as tall as the terminal scrolls its
/// own top away; the arithmetic that redraws the next frame then counts from
/// the wrong row, and the view duplicates itself down the screen for as long
/// as the run lasts. The tail is what survives because a live block's footer
/// is its last row.
#[test]
fn a_block_taller_than_the_terminal_keeps_its_tail() {
    let mut screen = tall(true, 40, 4);
    screen
        .draw(&lines(&["one", "two", "three", "four", "five", "six"]))
        .expect("draw");

    let drawn = screen.ui().contents();
    for gone in ["one", "two", "three"] {
        assert!(!drawn.contains(gone), "`{gone}` was drawn: {drawn:?}");
    }
    for kept in ["four", "five", "six"] {
        assert!(drawn.contains(kept), "`{kept}` was dropped: {drawn:?}");
    }
}

/// TC-UI-SCREEN-9: the terminal made shorter under a block already on it.
/// Expected: the next frame is held to the new height. A reader dragging a
/// window smaller mid-turn is the case this exists for, and the old height is
/// no guide to the new one.
#[test]
fn a_shorter_terminal_holds_the_next_frame() {
    let mut screen = tall(true, 40, 10);
    screen
        .draw(&lines(&["one", "two", "three", "four"]))
        .expect("draw");
    assert!(screen.ui().contents().contains("one"));

    screen.rows(3);
    screen
        .draw(&lines(&["five", "six", "seven"]))
        .expect("draw");

    let drawn = screen.ui().contents();
    let last = drawn.rsplit('\r').next().unwrap_or_default();
    assert!(
        !last.contains("five"),
        "three rows were drawn in two: {drawn:?}"
    );
    assert!(drawn.contains("seven"), "the tail is missing: {drawn:?}");
}
