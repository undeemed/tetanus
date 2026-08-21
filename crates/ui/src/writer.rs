//! The one place `tetanus` writes a line.
//!
//! Everything the binary prints goes through a [`Ui`]: it owns the stream, the
//! resolved [`Theme`], and the line width. Two consequences are the point of
//! the type. Renderers cannot reach `println!`, so no output can escape the
//! color policy; and a test builds a `Ui` over a `Vec<u8>` and asserts the
//! exact bytes, with no terminal and no environment involved.
//!
//! `Ui` is generic over its sink rather than boxed, so the test writer and the
//! locked stdout handle are the same code path.

use std::io::{self, IsTerminal, Write};

use crate::color::{self, ColorChoice, Env};
use crate::progress::Progress;
use crate::screen::Screen;
use crate::text::{or_empty, visible_width};
use crate::theme::{Painted, Role, Theme};

/// A styled output stream.
pub struct Ui<W> {
    out: W,
    theme: Theme,
    width: usize,
}

impl<W: Write> Ui<W> {
    /// Wrap a sink with an already-resolved policy. Tests use this directly.
    pub fn new(out: W, theme: Theme, width: usize) -> Self {
        Self { out, theme, width }
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn width(&self) -> usize {
        self.width
    }

    /// Draw at a new width. For a surface that repaints: the terminal a block
    /// is being drawn on can be resized while it is on screen.
    pub fn set_width(&mut self, width: usize) {
        self.width = width;
    }

    /// Style `text` as `role` for use inside a `write!` on this `Ui`.
    pub fn paint<'a>(&self, role: Role, text: &'a str) -> Painted<'a> {
        self.theme.paint(role, text)
    }

    /// Borrow the raw sink, for renderers that format their own line.
    pub fn out(&mut self) -> &mut W {
        &mut self.out
    }

    pub fn blank(&mut self) -> io::Result<()> {
        writeln!(self.out)
    }

    pub fn line(&mut self, text: &str) -> io::Result<()> {
        writeln!(self.out, "{text}")
    }

    /// A section title: blank line, then the title. The leading blank is the
    /// separator, so callers never hand-place one.
    pub fn heading(&mut self, text: &str) -> io::Result<()> {
        writeln!(self.out)?;
        writeln!(self.out, "{}", self.theme.paint(Role::Heading, text))
    }

    /// The same title, with the place the page read drawn muted beside it.
    ///
    /// A page that lists what is somewhere - the keys in a settings document,
    /// the journals under a sessions root - is answering a question about a
    /// place, and a reader who cannot see which place cannot act on the
    /// answer. Two pages ask for that shape, so it is one method rather than
    /// two compositions that drift apart.
    ///
    /// `place` is drawn as it is given, like [`line`](Self::line) and
    /// [`field`](Self::field): it comes off a document, a flag or an
    /// environment, so the caller tames it. It is never cut, because it is
    /// the one value on the page a reader copies - a terminal folding a long
    /// path leaves it readable, and a cut one sends them back to the flag
    /// they typed it on.
    pub fn heading_at(&mut self, text: &str, place: &str) -> io::Result<()> {
        writeln!(self.out)?;
        writeln!(
            self.out,
            "{}  {}",
            self.theme.paint(Role::Heading, text),
            self.theme.paint(Role::Muted, place)
        )
    }

    /// One `label  value` row. `pad` is the shared label column width, so a
    /// caller aligns a block by passing the same number for every row.
    ///
    /// The label is padded here rather than by a format width, because a
    /// format width counts characters: a label in a script a terminal draws
    /// twice as wide is fewer characters than the columns it takes, and every
    /// row under it would start somewhere else.
    ///
    /// Like [`line`](Self::line) and [`heading`](Self::heading), this draws
    /// what it is given. A renderer hands it a value it has already made
    /// safe: [`tame_line`](crate::tame_line) for one that came from outside,
    /// or its own paint. A row assembled from both cannot be told apart here,
    /// and taming it would take the paint out with the rest.
    ///
    /// A value that draws nothing is the one thing this does say for itself,
    /// in the renderer's own muted word rather than left out. A row that
    /// stopped after its label reads as a value the reader failed to see, and
    /// it would end in the blank space of the gap. Both cases reach here: a
    /// caller that had nothing to say, and one whose value was every
    /// character `tame_line` had to take out.
    pub fn field(&mut self, label: &str, pad: usize, value: &str) -> io::Result<()> {
        let gap = " ".repeat(pad.saturating_sub(visible_width(label)) + 2);
        let value = match visible_width(value) {
            0 => self.theme.paint(Role::Muted, or_empty(value)).to_string(),
            _ => value.to_string(),
        };
        writeln!(
            self.out,
            "{}{gap}{value}",
            self.theme.paint(Role::Muted, label)
        )
    }

    /// A full-width horizontal rule.
    pub fn rule(&mut self) -> io::Result<()> {
        let glyph = self.theme.glyph("─", "-");
        let rule = glyph.repeat(self.width);
        writeln!(self.out, "{}", self.theme.paint(Role::Muted, &rule))
    }

    /// `note: ...`, `warning: ...`, `error: ...` - the three diagnostic
    /// shapes, tagged the way `rustc` and `cargo` tag them so a user's eye
    /// already knows where to look.
    pub fn note(&mut self, text: &str) -> io::Result<()> {
        self.tagged(Role::Accent, "note", text)
    }

    pub fn warn(&mut self, text: &str) -> io::Result<()> {
        self.tagged(Role::Warn, "warning", text)
    }

    pub fn error(&mut self, text: &str) -> io::Result<()> {
        self.tagged(Role::Error, "error", text)
    }

    fn tagged(&mut self, role: Role, tag: &str, text: &str) -> io::Result<()> {
        writeln!(self.out, "{}: {text}", self.theme.paint(role, tag))
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// The resolved policy for both process streams, decided once at startup.
///
/// stdout and stderr are resolved separately on purpose: `tetanus run > log`
/// must keep the error report on the terminal readable and colored while the
/// captured stdout stays plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub stdout: Theme,
    pub stderr: Theme,
    pub width: usize,
    /// Whether stderr is a terminal. Colour does not answer this: `--color
    /// never` at a terminal is still a terminal, and progress may still
    /// repaint in place.
    pub stderr_is_terminal: bool,
    /// The same question for stdout, which the live view repaints on. The two
    /// are asked separately because they are redirected separately.
    pub stdout_is_terminal: bool,
    /// Whether stdout can hold a full-screen view: a terminal, and one that
    /// answers the cursor moves a screen is drawn with. A `--ui` flag is
    /// refused where this is false, because a page repainted at a terminal
    /// that cannot address its cursor arrives as the escapes themselves.
    pub stdout_is_screen: bool,
}

impl Policy {
    /// Resolve from a flag and an environment snapshot, asking each stream
    /// whether it is a terminal and the terminal how wide it is.
    pub fn resolve(choice: ColorChoice, env: &Env) -> Self {
        let charset = color::charset(env);
        let terminal_width = terminal_size::terminal_size().map(|(w, _)| w.0);
        let stderr_is_terminal = io::stderr().is_terminal();
        let stdout_is_terminal = io::stdout().is_terminal();
        Self {
            stdout: Theme::new(
                color::color_enabled(choice, env, io::stdout().is_terminal()),
                charset,
            ),
            stderr: Theme::new(
                color::color_enabled(choice, env, io::stderr().is_terminal()),
                charset,
            ),
            width: color::width(env, terminal_width),
            stderr_is_terminal,
            stdout_is_terminal,
            stdout_is_screen: stdout_is_terminal && color::addressable(env),
        }
    }

    /// Read the process environment and resolve. What `main` calls.
    pub fn from_process(choice: ColorChoice) -> Self {
        Self::resolve(choice, &Env::from_process())
    }

    pub fn stdout(&self) -> Ui<io::Stdout> {
        Ui::new(io::stdout(), self.stdout, self.width)
    }

    pub fn stderr(&self) -> Ui<io::Stderr> {
        Ui::new(io::stderr(), self.stderr, self.width)
    }

    /// The progress line, on stderr, animated only at a terminal.
    pub fn stderr_progress(&self) -> Progress<io::Stderr> {
        Progress::new(self.stderr(), self.stderr_is_terminal)
    }

    /// The live block, on stdout, repainted only at a terminal. A piped run
    /// gets the printed lines and no frames at all.
    pub fn stdout_screen(&self) -> Screen<io::Stdout> {
        Screen::new(self.stdout(), self.stdout_is_terminal)
    }
}

/// The width right now, asked again rather than remembered.
///
/// [`Policy`] resolves a width once, which is all a command that prints and
/// exits needs. A block that stays on screen outlives that answer: the window
/// it is drawn in can be resized under it, and a block still fitted to the old
/// width wraps and takes the frame after it out of place.
///
/// The rules are the ones the policy used, so `COLUMNS` still wins - a user
/// who exports it means it, and a resize does not change their mind.
pub fn measure() -> usize {
    let terminal = terminal_size::terminal_size().map(|(w, _)| w.0);
    color::width(&Env::from_process(), terminal)
}

/// A `Ui` over an in-memory buffer, for tests and doc examples.
pub fn buffered(theme: Theme, width: usize) -> Ui<Vec<u8>> {
    Ui::new(Vec::new(), theme, width)
}

impl Ui<Vec<u8>> {
    /// What was written, as text.
    pub fn contents(&self) -> String {
        String::from_utf8_lossy(&self.out).into_owned()
    }
}
