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

    /// One `label  value` row. `pad` is the shared label column width, so a
    /// caller aligns a block by passing the same number for every row.
    pub fn field(&mut self, label: &str, pad: usize, value: &str) -> io::Result<()> {
        writeln!(
            self.out,
            "{:<pad$}  {}",
            self.theme.paint(Role::Muted, label),
            value
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
}

impl Policy {
    /// Resolve from a flag and an environment snapshot, asking each stream
    /// whether it is a terminal and the terminal how wide it is.
    pub fn resolve(choice: ColorChoice, env: &Env) -> Self {
        let charset = color::charset(env);
        let terminal_width = terminal_size::terminal_size().map(|(w, _)| w.0);
        let stderr_is_terminal = io::stderr().is_terminal();
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
