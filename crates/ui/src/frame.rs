//! One whole screen, composed and then painted in a single pass.
//!
//! [`Screen`](crate::Screen) owns a block on the ordinary screen: it redraws
//! the rows it wrote and leaves everything above them alone, so the scrollback
//! keeps growing under it. That is right for a command whose output a person
//! scrolls back through afterwards, and wrong for a view on the alternate
//! screen, where the canvas is the whole terminal and there is no scrollback
//! to protect.
//!
//! So the unit here is the frame, not the block. A view builds every row it
//! wants, top to bottom, and paints the lot. No row is diffed against the row
//! under it: at the sizes a terminal has, writing the rows costs less than
//! deciding which of them changed, and a repaint that cannot disagree with the
//! screen is one class of bug that does not exist.
//!
//! A whole frame is a different question, and the type answers it by being
//! comparable. A view driven by a clock rather than by a keystroke composes a
//! frame every time the clock comes round, whether or not anything has
//! happened, and once its subject stops changing every one of those frames is
//! the frame already on the terminal. Comparing two frames is cheap and cannot
//! disagree with the screen either, because the screen is the last frame that
//! was painted. See `Watch::paint` in the binary, the one view with a clock.
//!
//! # What the type guarantees
//!
//! - **Exactly `rows` rows.** Short of that it pads, past it it drops. A view
//!   that miscounts draws a wrong frame; it never scrolls the terminal, which
//!   would move every row it drew afterwards.
//! - **No row is wider than `cols`.** Cut by what a terminal draws rather than
//!   by bytes, keeping the SGR sequences a theme wrote in - see [`fit`].
//! - **No line terminator after the last row.** Writing one on the bottom row
//!   scrolls the terminal by one, which is the same defect from the other
//!   side.
//! - **`\r\n` between rows, never a bare `\n`.** In raw mode a line feed moves
//!   down and does not return to column zero, so a frame written with `\n`
//!   comes out as a staircase. This is the single most common way a first
//!   full-screen view looks broken.
//!
//! # Where the size comes from
//!
//! The caller. A frame is built for the size the terminal had when the view
//! last heard of it, and the next frame is simply built at the new size. There
//! is nothing to reflow, because nothing was kept.

use std::io::{self, Write};

use crate::text::fit;
use crate::writer::Ui;

/// A screen's worth of rows, waiting to be painted.
///
/// Comparable, so that a view whose clock runs faster than its content changes
/// can tell that the screen it is about to paint is the screen already on the
/// terminal. Painting it again would be the same bytes for the same picture.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    cols: usize,
    rows: usize,
    lines: Vec<String>,
}

impl Frame {
    /// An empty frame for a terminal of this size.
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            lines: Vec::with_capacity(rows),
        }
    }

    /// The width every row is cut to.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// The height the frame will occupy, whatever was added to it.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Rows not yet used.
    ///
    /// What a view asks before deciding how much of a transcript to show, so
    /// the decision is made where the content is rather than by a silent cut
    /// down here.
    pub fn free(&self) -> usize {
        self.rows.saturating_sub(self.lines.len())
    }

    /// Add one row.
    ///
    /// A row past the bottom of the screen is dropped rather than pushing the
    /// frame taller. The alternative is a frame that scrolls, and a scrolled
    /// frame puts every row of the next one in the wrong place.
    pub fn row(&mut self, text: impl AsRef<str>) {
        if self.free() == 0 {
            return;
        }
        self.lines.push(text.as_ref().to_string());
    }

    /// Add a blank row.
    pub fn blank(&mut self) {
        self.row("");
    }

    /// Write the frame to the terminal, in one pass from the top left.
    pub fn paint<W: Write>(&self, ui: &mut Ui<W>) -> io::Result<()> {
        let charset = ui.theme().charset();
        let out = ui.out();
        // Home, rather than counting rows back up from wherever the cursor
        // ended: the frame is the whole screen, so its first row is the
        // terminal's first row by definition.
        write!(out, "\x1b[H")?;
        for row in 0..self.rows {
            if row > 0 {
                // Carriage return as well as line feed. Raw mode does not
                // supply the return, and without it row two starts under the
                // end of row one.
                write!(out, "\r\n")?;
            }
            let text = self
                .lines
                .get(row)
                .map(|line| fit(line, self.cols, charset))
                .unwrap_or_default();
            // Erase to the end of the row rather than padding it with spaces:
            // it is fewer bytes, and it does not repaint a background colour
            // over the part of the row nobody wrote to.
            write!(out, "{text}\x1b[K")?;
        }
        // Anything below the frame belongs to a taller frame that came before
        // - a terminal that was made shorter. The cursor is on the last row,
        // so this erases from there down.
        write!(out, "\x1b[J")?;
        ui.flush()
    }
}

/// Test Design Specification: composing and painting one screen.
///
/// Features tested: that a frame paints exactly its own height whatever was
/// added to it; that rows are cut to the width by what a terminal draws; that
/// rows past the bottom are dropped; that rows are separated by `\r\n` and
/// never by a bare `\n`; that nothing follows the last row; and that two
/// frames are equal exactly when they would paint the same screen.
///
/// Features NOT tested here: the cut itself (owned by `text::fit`, asserted in
/// `tests/text.rs`), the colour policy (owned by `theme`), and getting into
/// the alternate screen in the first place (owned by `terminal`).
///
/// Environmental needs: none. Every case paints into a buffer.
#[cfg(test)]
mod tests {
    use crate::color::Charset;
    use crate::theme::Theme;
    use crate::writer::buffered;

    use super::*;

    fn painted(frame: &Frame) -> String {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), frame.cols());
        frame.paint(&mut ui).expect("paint");
        ui.contents()
    }

    /// TC-UI-FRAME-1: a frame with rows in it.
    /// Expected: home, then each row followed by an erase-to-end-of-line,
    /// separated by `\r\n`, then an erase below. Asserted as exact bytes
    /// because every part of it is load-bearing: drop the `\r` and the frame
    /// is a staircase, drop the `\x1b[K` and a shorter row leaves the tail of
    /// the last one on screen.
    #[test]
    fn a_frame_is_painted_in_one_pass_from_the_top_left() {
        let mut frame = Frame::new(10, 3);
        frame.row("one");
        frame.row("two");
        frame.row("three");

        assert_eq!(
            painted(&frame),
            "\x1b[Hone\x1b[K\r\ntwo\x1b[K\r\nthree\x1b[K\x1b[J"
        );
    }

    /// TC-UI-FRAME-2: fewer rows than the terminal has.
    /// Expected: the frame still occupies its full height, the unused rows
    /// erased. A view that drew four rows into a twenty-row terminal and left
    /// the other sixteen alone would be showing the previous frame under the
    /// current one.
    #[test]
    fn an_unfilled_frame_still_occupies_the_whole_screen() {
        let mut frame = Frame::new(10, 4);
        frame.row("one");

        assert_eq!(
            painted(&frame),
            "\x1b[Hone\x1b[K\r\n\x1b[K\r\n\x1b[K\r\n\x1b[K\x1b[J"
        );
        assert_eq!(frame.free(), 3);
    }

    /// TC-UI-FRAME-3: more rows than the terminal has.
    /// Expected: the extra rows are dropped and the height is unchanged. The
    /// alternative is a frame that scrolls, which puts every row of the next
    /// frame one line out of place - a view that looks fine for one frame and
    /// then walks off the screen.
    #[test]
    fn rows_past_the_bottom_are_dropped_rather_than_scrolling() {
        let mut frame = Frame::new(10, 2);
        for row in ["one", "two", "three", "four"] {
            frame.row(row);
        }

        assert_eq!(painted(&frame), "\x1b[Hone\x1b[K\r\ntwo\x1b[K\x1b[J");
        assert_eq!(frame.free(), 0);
    }

    /// TC-UI-FRAME-4: a row wider than the terminal.
    /// Expected: cut to the width, with the mark that says it was cut. A row
    /// that overran would wrap onto the next one, and from there the whole
    /// frame is one row out.
    #[test]
    fn a_row_is_cut_to_the_width() {
        let mut frame = Frame::new(6, 1);
        frame.row("a long row indeed");

        assert_eq!(painted(&frame), "\x1b[Ha lon\u{2026}\x1b[K\x1b[J");
    }

    /// TC-UI-FRAME-5: the two terminator rules, stated as their own case
    /// because both are silent failures rather than errors.
    /// Expected: no bare line feed anywhere - in raw mode it moves down
    /// without returning, so a frame written with `\n` comes out as a
    /// staircase - and nothing at all after the last row, because a line
    /// terminator on the bottom row scrolls the terminal by one.
    #[test]
    fn rows_are_separated_by_a_return_and_the_last_one_ends_the_frame() {
        let mut frame = Frame::new(10, 3);
        frame.row("one");
        frame.row("two");
        frame.row("three");
        let told = painted(&frame);

        assert_eq!(told.matches('\n').count(), told.matches("\r\n").count());
        assert_eq!(told.matches("\r\n").count(), 2, "{told:?}");
        assert!(told.ends_with("three\x1b[K\x1b[J"), "{told:?}");
    }

    /// TC-UI-FRAME-7: two frames that would paint the same screen.
    /// Expected: equal, and unequal as soon as a row, the row count or the
    /// size differs. A view with a clock skips the paint when the frame it
    /// composed equals the one on the terminal, so an equality that missed a
    /// change would leave the screen saying something that is no longer true.
    #[test]
    fn frames_that_paint_the_same_screen_are_equal() {
        let build = |cols, rows, lines: &[&str]| {
            let mut frame = Frame::new(cols, rows);
            for line in lines {
                frame.row(line);
            }
            frame
        };

        assert_eq!(build(10, 3, &["one", "two"]), build(10, 3, &["one", "two"]));
        assert_ne!(build(10, 3, &["one", "two"]), build(10, 3, &["one", "TWO"]));
        assert_ne!(build(10, 3, &["one", "two"]), build(10, 3, &["one"]));
        assert_ne!(build(10, 3, &["one", "two"]), build(11, 3, &["one", "two"]));
        assert_ne!(build(10, 3, &["one", "two"]), build(10, 4, &["one", "two"]));
    }

    /// TC-UI-FRAME-6: a terminal with no room at all.
    /// Expected: the cursor goes home, the screen is erased, and nothing is
    /// written. A window dragged to nothing is a real state, and the frame
    /// arithmetic must not underflow on the way through it.
    #[test]
    fn a_frame_with_no_rows_erases_and_writes_nothing() {
        let mut frame = Frame::new(0, 0);
        frame.row("nowhere to put this");

        assert_eq!(painted(&frame), "\x1b[H\x1b[J");
        assert_eq!(frame.free(), 0);
    }
}
