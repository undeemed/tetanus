//! The palette, addressed by role rather than by color.
//!
//! Call sites name what a span *is* - a heading, a durable sequence number, a
//! tool - and never which SGR code it gets. That is what keeps one palette
//! change from turning into an edit across the CLI, and it is what lets the
//! whole surface degrade to plain text by flipping a single flag.
//!
//! Only the 8 ANSI base colors and `bold`/`dim` are used. They are the ones a
//! user's terminal theme can actually correct for; hard-coded 24-bit colors
//! are how a CLI ends up unreadable on a light background.

use std::fmt;

use anstyle::{AnsiColor, Style};

use crate::color::Charset;

/// What a span of output means. See the module docs for why this is not a color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Section titles.
    Heading,
    /// The quiet half of a line: field labels, hints, provenance, counts.
    Muted,
    /// The one thing on the line the eye should land on.
    Accent,
    /// A durable journal sequence number.
    Seq,
    /// An event topic.
    Topic,
    /// A tool name.
    Tool,
    Ok,
    Warn,
    Error,
}

impl Role {
    fn style(self) -> Style {
        let plain = Style::new();
        match self {
            Self::Heading => plain.bold(),
            Self::Muted => plain.dimmed(),
            Self::Accent => plain.fg_color(Some(AnsiColor::Cyan.into())).bold(),
            Self::Seq => plain.fg_color(Some(AnsiColor::Blue.into())),
            Self::Topic => plain.fg_color(Some(AnsiColor::Cyan.into())),
            Self::Tool => plain.fg_color(Some(AnsiColor::Magenta.into())),
            Self::Ok => plain.fg_color(Some(AnsiColor::Green.into())),
            Self::Warn => plain.fg_color(Some(AnsiColor::Yellow.into())),
            Self::Error => plain.fg_color(Some(AnsiColor::Red.into())).bold(),
        }
    }
}

/// A resolved presentation policy for one stream: whether it gets color, and
/// which glyphs it may draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    color: bool,
    charset: Charset,
}

impl Theme {
    pub fn new(color: bool, charset: Charset) -> Self {
        Self { color, charset }
    }

    /// No color, ASCII glyphs. The shape every test asserts against.
    pub fn plain() -> Self {
        Self::new(false, Charset::Ascii)
    }

    pub fn color(&self) -> bool {
        self.color
    }

    pub fn charset(&self) -> Charset {
        self.charset
    }

    /// Style `text` as `role`. With color off this is the text itself, byte
    /// for byte, so piped output is never "colored text with the codes
    /// removed" - it is the same string the renderer measured for alignment.
    pub fn paint<'a>(&self, role: Role, text: &'a str) -> Painted<'a> {
        Painted {
            style: self.color.then(|| role.style()),
            text,
        }
    }

    /// Pick between a Unicode glyph and its ASCII stand-in.
    pub fn glyph(&self, unicode: &'static str, ascii: &'static str) -> &'static str {
        match self.charset {
            Charset::Unicode => unicode,
            Charset::Ascii => ascii,
        }
    }
}

/// Text plus an optional style, rendered on `Display`.
///
/// The reset is emitted only when a style was, so a plain theme adds no bytes.
pub struct Painted<'a> {
    style: Option<Style>,
    text: &'a str,
}

impl fmt::Display for Painted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Width and precision apply to the text, never to the escape
        // sequences: `{:>8}` on a colored span must align with a plain one.
        match self.style {
            Some(style) => {
                write!(f, "{}", style.render())?;
                f.pad(self.text)?;
                write!(f, "{}", style.render_reset())
            }
            None => f.pad(self.text),
        }
    }
}
