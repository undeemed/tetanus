//! A block of lines that is redrawn in place.
//!
//! [`Progress`](crate::Progress) owns one line and repaints it with a carriage
//! return, which is why it never writes an escape code. A live view of a turn
//! needs more than one line - the answer as it arrives, the tool that is
//! running, the footer under both - and no carriage return moves a cursor up a
//! row. So this is the one type in the crate that writes cursor escapes, and
//! it writes them only when the caller said the stream is a terminal.
//!
//! Two behaviours, the same split as `Progress`:
//!
//! - **Animated**: [`Screen::draw`] replaces the block below the cursor, in
//!   place, as often as the caller likes.
//! - **Plain**: [`Screen::draw`] writes nothing whatsoever. A pipe receives
//!   only what [`Screen::print`] commits, so a piped run stays escape-free and
//!   stays byte-identical to the same run with no terminal attached.
//!
//! The two methods differ in what they promise, not in where they write. A
//! printed line has left the block for good and will never be rewritten; a
//! drawn line is transient and true only until the next frame. A caller that
//! wants its last frame kept prints it before dropping the screen.
//!
//! A terminal can be resized while a block is on it. [`Screen::resize`] is how
//! a caller says so: the block on screen belongs to the old width, so it is
//! erased before the first frame of the new one.
//!
//! # Why a block never fills the terminal
//!
//! The cursor arithmetic assumes the block did not scroll. A block as tall as
//! the terminal scrolls its own top away, and every frame after it is drawn
//! one row out of place - the failure looks like the view duplicating itself
//! down the screen, and it does not stop until the run does.
//!
//! Callers keep the block short for their own reasons - a live view shows the
//! tail of a turn, not the whole of it - but "short" is a number they chose
//! against a terminal they cannot see. This type knows the terminal, so this
//! type holds the block to it: the last rows of what it was handed, one row
//! kept clear for the shell prompt the cursor is sitting on. The tail is what
//! survives, because the footer of a live block is its last row and the rows
//! above it are the answer scrolling past.
//!
//! The transcript above the block is printed rather than drawn, and printing
//! is what is allowed to scroll.

use std::io::{self, Write};

use crate::text::fit;
use crate::writer::Ui;

/// A redrawable block of lines over a stream.
pub struct Screen<W: Write> {
    ui: Ui<W>,
    animated: bool,
    /// Rows of the block currently on the terminal, to be replaced or erased
    /// on the next frame.
    drawn: usize,
    /// Rows the terminal has, as the caller last measured it.
    rows: usize,
}

impl<W: Write> Screen<W> {
    /// Wrap a stream. `animated` is "this stream is a terminal", which the
    /// caller resolves once - see [`Policy::stdout_screen`](crate::Policy).
    pub fn new(ui: Ui<W>, animated: bool, rows: usize) -> Self {
        Self {
            ui,
            animated,
            drawn: 0,
            rows,
        }
    }

    /// The terminal is this many rows now.
    ///
    /// Told separately from the width, because the two arrive together and
    /// mean different things: a width that changed invalidates the block on
    /// screen, and a height that changed only decides how much of the next one
    /// there is room for.
    pub fn rows(&mut self, rows: usize) {
        self.rows = rows;
    }

    /// Commit lines above the block. They scroll with the terminal and are
    /// never rewritten.
    ///
    /// This is the whole of the output in plain mode, which is why a caller
    /// commits every line it wants a piped run to keep.
    pub fn print(&mut self, lines: &[String]) -> io::Result<()> {
        self.clear()?;
        for line in lines {
            self.ui.line(line)?;
        }
        self.ui.flush()
    }

    /// Replace the block. A no-op in plain mode.
    pub fn draw(&mut self, lines: &[String]) -> io::Result<()> {
        if !self.animated {
            return Ok(());
        }
        let width = self.ui.width();
        let charset = self.ui.theme().charset();
        // The tail of what was handed over, and never the whole terminal: one
        // row is left for the prompt the cursor is on, and a block that filled
        // the screen would scroll its own top away.
        let room = self.rows.saturating_sub(1).max(1);
        let lines = &lines[lines.len().saturating_sub(room)..];
        self.home()?;
        for line in lines {
            // A line that wrapped would occupy two rows, and every frame after
            // it would be drawn one row out of place. Cutting is what keeps
            // one line one row.
            let text = fit(line, width, charset);
            writeln!(self.ui.out(), "{text}\x1b[K")?;
        }
        // The old block may have been taller. Erase what is left of it, then
        // come back to the row under the new block.
        let extra = self.drawn.saturating_sub(lines.len());
        for _ in 0..extra {
            writeln!(self.ui.out(), "\x1b[K")?;
        }
        if extra > 0 {
            write!(self.ui.out(), "\x1b[{extra}A")?;
        }
        self.drawn = lines.len();
        self.ui.flush()
    }

    /// Draw at a new width from the next frame on.
    ///
    /// The block on screen was fitted to the old width, and a terminal that
    /// reflowed it while narrowing has already put the rows somewhere this
    /// type cannot compute. So the old block is erased with the arithmetic
    /// that still describes it, and the next frame starts from a clean row.
    /// Nothing happens when the width has not moved, which is every frame but
    /// the one after a resize.
    pub fn resize(&mut self, width: usize) -> io::Result<()> {
        if width == self.ui.width() {
            return Ok(());
        }
        self.clear()?;
        self.ui.set_width(width);
        Ok(())
    }

    /// Erase the block, leaving the cursor where it began.
    pub fn clear(&mut self) -> io::Result<()> {
        if self.drawn == 0 {
            return Ok(());
        }
        self.home()?;
        write!(self.ui.out(), "\x1b[J")?;
        self.drawn = 0;
        self.ui.flush()
    }

    /// Erase the block and hand the stream back. The caller writes its own
    /// last word, so this type never decides what a finished turn looks like.
    pub fn finish(mut self) -> io::Result<Ui<W>> {
        self.clear()?;
        Ok(self.ui)
    }

    pub fn ui(&self) -> &Ui<W> {
        &self.ui
    }

    /// Put the cursor in column zero of the block's first row.
    fn home(&mut self) -> io::Result<()> {
        if self.drawn > 0 {
            write!(self.ui.out(), "\x1b[{}A", self.drawn)?;
        }
        write!(self.ui.out(), "\r")
    }
}
