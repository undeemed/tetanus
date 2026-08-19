//! The line that says something is still happening.
//!
//! A turn can spend a long time inside one model call, and a surface that
//! prints nothing until it finishes looks hung. [`Progress`] is the one place
//! that says otherwise, and it has exactly two behaviours:
//!
//! - **Animated**, when the stream is a terminal: one line, repainted in
//!   place, never scrolling. Repainting is done with a carriage return and
//!   spaces, never with a cursor escape, so a plain theme stays escape-free.
//! - **Plain**, when it is not: one line per phase, and nothing at all while a
//!   phase lasts. A CI log wants the sequence of phases, not sixty frames of a
//!   spinner.
//!
//! Progress belongs on stderr. Keeping it off stdout is what lets
//! `tetanus run | cat` produce the same bytes whether or not anyone was
//! watching it run.

use std::io::{self, Write};

use crate::color::Charset;
use crate::theme::Role;
use crate::writer::Ui;

const SPIN_UNICODE: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPIN_ASCII: &[&str] = &["-", "\\", "|", "/"];

/// A single status line over a stream.
pub struct Progress<W: Write> {
    ui: Ui<W>,
    animated: bool,
    frames: &'static [&'static str],
    frame: usize,
    label: Option<String>,
    /// Columns currently dirty on the terminal line, to be erased on redraw.
    dirty: usize,
}

impl<W: Write> Progress<W> {
    /// Wrap a stream. `animated` is "this stream is a terminal", which the
    /// caller resolves once - see `Policy::stderr_progress`.
    pub fn new(ui: Ui<W>, animated: bool) -> Self {
        let frames = match ui.theme().charset() {
            Charset::Unicode => SPIN_UNICODE,
            Charset::Ascii => SPIN_ASCII,
        };
        Self {
            ui,
            animated,
            frames,
            frame: 0,
            label: None,
            dirty: 0,
        }
    }

    /// Announce a phase. In plain mode a repeated label writes nothing, so a
    /// caller may call this as often as it likes.
    pub fn set(&mut self, label: &str) -> io::Result<()> {
        if self.label.as_deref() == Some(label) {
            return if self.animated { self.draw() } else { Ok(()) };
        }
        self.label = Some(label.to_string());
        if self.animated {
            self.draw()
        } else {
            let line = self.ui.paint(Role::Muted, label).to_string();
            self.ui.line(&line)
        }
    }

    /// Advance the animation without changing the phase. A no-op when plain.
    pub fn tick(&mut self) -> io::Result<()> {
        if !self.animated || self.label.is_none() {
            return Ok(());
        }
        self.frame = self.frame.wrapping_add(1);
        self.draw()
    }

    /// Erase the status line, leaving the cursor where it started.
    pub fn clear(&mut self) -> io::Result<()> {
        if self.dirty == 0 {
            return Ok(());
        }
        let blanks = " ".repeat(self.dirty);
        write!(self.ui.out(), "\r{blanks}\r")?;
        self.dirty = 0;
        self.ui.flush()
    }

    /// Erase the status line and hand the stream back. The caller writes its
    /// own last word, so this type never decides what "done" says.
    pub fn finish(mut self) -> io::Result<Ui<W>> {
        self.clear()?;
        Ok(self.ui)
    }

    pub fn ui(&self) -> &Ui<W> {
        &self.ui
    }

    fn draw(&mut self) -> io::Result<()> {
        let Some(label) = self.label.clone() else {
            return Ok(());
        };
        let glyph = self.frames[self.frame % self.frames.len()];
        let charset = self.ui.theme().charset();
        let text = fit(&format!("{glyph} {label}"), self.ui.width(), charset);
        let cols = text.chars().count();
        let painted = self.ui.paint(Role::Accent, &text).to_string();

        if self.dirty > 0 {
            let blanks = " ".repeat(self.dirty);
            write!(self.ui.out(), "\r{blanks}")?;
        }
        write!(self.ui.out(), "\r{painted}")?;
        self.dirty = cols;
        self.ui.flush()
    }
}

/// Cut `text` to `width` columns, marking that it was cut.
///
/// A status line that wraps is worse than one that is too short: the terminal
/// scrolls, and the repaint then lands on the wrong row.
fn fit(text: &str, width: usize, charset: Charset) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mark = match charset {
        Charset::Unicode => "…",
        Charset::Ascii => "...",
    };
    let keep = width.saturating_sub(mark.chars().count());
    text.chars().take(keep).chain(mark.chars()).collect()
}
