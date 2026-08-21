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
//!
//! # Why an animated line counts the seconds
//!
//! A spinner says the process is alive. It does not say whether this call has
//! been running for four seconds or four minutes, and that is the question a
//! reader actually has while they wait: a model that is slow today and a
//! model that will never answer look identical for as long as nobody counts.
//! So the animated line carries the time since the phase began, once there is
//! enough of it to be worth reading.
//!
//! The plain line does not. A log gets one line per phase and no frames, and
//! a duration written into it would be a duration measured from the last
//! phase change rather than from anything the log's reader can see - and it
//! would make two runs of the same turn print different bytes.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crate::color::Charset;
use crate::text::{truncate, visible_width};
use crate::theme::Role;
use crate::writer::Ui;

const SPIN_UNICODE: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPIN_ASCII: &[&str] = &["-", "\\", "|", "/"];

/// One frame of the spinner, for a surface that has to say "still working"
/// inside a layout of its own rather than on the status line.
///
/// The frames live here because there is one spinner in this binary, not one
/// per view: a live block and a status line that disagreed about what waiting
/// looks like would read as two programs.
pub fn frame(charset: Charset, tick: usize) -> &'static str {
    let frames = match charset {
        Charset::Unicode => SPIN_UNICODE,
        Charset::Ascii => SPIN_ASCII,
    };
    frames[tick % frames.len()]
}

/// A single status line over a stream.
pub struct Progress<W: Write> {
    ui: Ui<W>,
    animated: bool,
    frame: usize,
    label: Option<String>,
    /// Columns currently dirty on the terminal line, to be erased on redraw.
    dirty: usize,
    /// When the current phase began, for the time the animated line carries.
    /// `None` until a phase is announced.
    since: Option<Instant>,
}

/// How long a phase runs before the line starts counting.
///
/// Under this, a duration is noise: every offline turn finishes inside it, and
/// a line that flashed `0s` and vanished says less than one that did not.
const QUIET: Duration = Duration::from_secs(2);

impl<W: Write> Progress<W> {
    /// Wrap a stream. `animated` is "this stream is a terminal", which the
    /// caller resolves once - see `Policy::stderr_progress`.
    pub fn new(ui: Ui<W>, animated: bool) -> Self {
        Self {
            ui,
            animated,
            frame: 0,
            label: None,
            dirty: 0,
            since: None,
        }
    }

    /// Announce a phase. In plain mode a repeated label writes nothing, so a
    /// caller may call this as often as it likes.
    pub fn set(&mut self, label: &str) -> io::Result<()> {
        if self.label.as_deref() == Some(label) {
            return if self.animated { self.draw() } else { Ok(()) };
        }
        self.label = Some(label.to_string());
        // The clock is the phase's, not the line's: a turn that reaches its
        // second step starts counting that step, because that is the wait the
        // reader is now in.
        self.since = Some(Instant::now());
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
        let spent = self.since.map(|since| since.elapsed()).unwrap_or_default();
        self.paint(spent)
    }

    /// Draw the line for a phase that has been running `spent`.
    ///
    /// Split from [`Progress::draw`] so the wording of a wait is a pure
    /// function of how long it has been: a case states the duration it means
    /// instead of sleeping for it.
    fn paint(&mut self, spent: Duration) -> io::Result<()> {
        let Some(label) = self.label.clone() else {
            return Ok(());
        };
        // A plain stream had its line when the phase was announced. Drawing
        // again here would be a second line per frame down a log.
        if !self.animated {
            return Ok(());
        }
        let charset = self.ui.theme().charset();
        let glyph = frame(charset, self.frame);
        let waited = match spent >= QUIET {
            true => format!(" {} {}", theme_dot(charset), waited(spent)),
            false => String::new(),
        };
        let text = truncate(
            &format!("{glyph} {label}{waited}"),
            self.ui.width(),
            charset,
        );
        // Columns, not characters. A label the terminal draws two columns
        // wide per character - a model named in Chinese, an emoji in a tool's
        // name - would otherwise be erased with too few spaces, and its tail
        // would stay on the line under whatever the caller wrote next.
        let cols = visible_width(&text);
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

/// The separator between the phase and its clock, in each charset. The same
/// one the closing line of a turn puts between its parts.
fn theme_dot(charset: Charset) -> &'static str {
    match charset {
        Charset::Unicode => "·",
        Charset::Ascii => "-",
    }
}

/// How long a wait has been, as a reader would say it.
///
/// Seconds while there are fewer than a hundred of them, because that is the
/// number a reader is comparing against their own patience; minutes and
/// seconds after that, because "412s" is a number nobody reads as seven
/// minutes. Never milliseconds: a line redrawn twelve times a second with a
/// millisecond on it is a line that cannot be read at all.
fn waited(spent: Duration) -> String {
    let seconds = spent.as_secs();
    match seconds < 100 {
        true => format!("{seconds}s"),
        false => format!("{}m{:02}s", seconds / 60, seconds % 60),
    }
}

/// Test Design Specification: the clock on an animated status line.
///
/// Features tested: the wording of a wait at each scale, that a line stays
/// quiet until a wait is worth reading, that the separator follows the
/// charset, and that a plain stream never carries a clock.
///
/// Features NOT tested here: what the animated line writes and erases (owned
/// by `tests/progress.rs`, which drives the public calls), and the passage of
/// time itself - every case states the duration it means rather than sleeping
/// for it, which is why the drawing is split from the clock that reads it.
///
/// Environmental needs: none. Every case writes into a `Vec<u8>`.
#[cfg(test)]
mod tests {
    use crate::writer::buffered;
    use crate::Theme;

    use super::*;

    fn line(charset: Charset, animated: bool, spent: Duration) -> String {
        let mut progress = Progress::new(buffered(Theme::new(false, charset), 60), animated);
        progress
            .set("running the turn on mock-echo-1")
            .expect("set");
        progress.paint(spent).expect("paint");
        progress.ui().contents()
    }

    /// TC-UI-PROG-8: the wording of a wait, at each scale it can reach.
    /// Expected: seconds while there are fewer than a hundred of them, and
    /// minutes and seconds after that. `412s` is a number nobody reads as
    /// seven minutes, and a millisecond on a line redrawn twelve times a
    /// second cannot be read at all.
    #[test]
    fn a_wait_is_worded_at_the_scale_it_reached() {
        assert_eq!(waited(Duration::from_secs(2)), "2s");
        assert_eq!(waited(Duration::from_millis(59_900)), "59s");
        assert_eq!(waited(Duration::from_secs(99)), "99s");
        assert_eq!(waited(Duration::from_secs(100)), "1m40s");
        assert_eq!(waited(Duration::from_secs(412)), "6m52s");
        assert_eq!(waited(Duration::from_secs(3600)), "60m00s");
    }

    /// TC-UI-PROG-9: a phase that has just begun, and one that has not.
    /// Expected: no clock under two seconds, a clock at two and after. Every
    /// offline turn finishes inside that, and a line that flashed `0s` and
    /// vanished says less than one that did not.
    #[test]
    fn the_line_is_quiet_until_the_wait_is_worth_reading() {
        let early = line(Charset::Unicode, true, Duration::from_millis(1_900));
        assert!(!early.contains('s'), "a clock too early: {early:?}");
        assert!(early.contains("running the turn"), "no phase: {early:?}");

        let waited = line(Charset::Unicode, true, Duration::from_secs(2));
        assert!(waited.ends_with("· 2s"), "no clock: {waited:?}");
    }

    /// TC-UI-PROG-10: the separator, in each charset.
    /// Expected: the dot a Unicode terminal gets and the hyphen an ASCII one
    /// gets - the same pair every other line of this binary chooses between,
    /// so a terminal that cannot draw a dot never gets one drawn.
    #[test]
    fn the_separator_follows_the_charset() {
        let unicode = line(Charset::Unicode, true, Duration::from_secs(5));
        assert!(unicode.contains("· 5s"), "{unicode:?}");

        let ascii = line(Charset::Ascii, true, Duration::from_secs(5));
        assert!(ascii.contains("- 5s"), "{ascii:?}");
        assert!(!ascii.contains('·'), "a dot on an ASCII line: {ascii:?}");
    }

    /// TC-UI-PROG-11: a plain stream, at a duration that would carry a clock.
    /// Expected: the phase, once, and no clock. A log wants the sequence of
    /// phases; a duration in it would be measured from the last phase change
    /// rather than from anything its reader can see, and it would make two
    /// runs of one turn print different bytes.
    #[test]
    fn a_plain_stream_gets_no_clock() {
        let plain = line(Charset::Unicode, false, Duration::from_secs(30));

        assert_eq!(plain, "running the turn on mock-echo-1\n");
    }
}
