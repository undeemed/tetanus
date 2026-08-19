//! The journals this build has written, as a page with a cursor on it.
//!
//! `tetanus sessions` prints a list whose only use is to be read an id or a
//! path out of. A directory with a hundred journals in it prints as a hundred
//! lines poured into the scrollback, and the one wanted is somewhere in the
//! middle of them. This module is the other view of the same list: the
//! alternate screen, a window on as much of it as fits, and a cursor to move
//! through it with.
//!
//! # Stakeholders and concerns
//!
//! - *A person looking for a turn from earlier*: can I find it without
//!   scrolling my shell back through everything else I have run?
//! - *The presentation lane*: is there one composer of a session row, or two?
//! - *A reviewer of the crate seam*: what does this add to `tetanus-ui`?
//!
//! # Composition
//!
//! ```text
//! pick   ── composes the list, hands it to the loop, answers how it ended
//! Picker ── the View: the rows, a window on them, and a cursor
//! ```
//!
//! Nothing here composes a row. Every row comes from
//! [`sessions::rows`](super::sessions::rows), the ones `tetanus sessions`
//! prints, so the picker cannot disagree with the command about what it is
//! showing. Nothing here holds the terminal or reads a key either: [`show`]
//! does both, and this module is the view it drives.
//!
//! # Rationale: the window follows the cursor
//!
//! [`Page`](tetanus_ui::Page) moves a window through a transcript, which is
//! the right arrangement for lines arriving and the wrong one for rows being
//! chosen between: what has to stay on the screen is not the newest row but
//! the one under the cursor. So this view keeps its own first row shown and
//! moves it only when the cursor would otherwise leave the screen, which is
//! what makes a held Down key read as a list sliding past rather than as a
//! cursor jumping a screen at a time.
//!
//! # Rationale: a mark, not only a colour
//!
//! The cursor is `› ` in Unicode and `> ` in ASCII, and every row is composed
//! two columns narrower to leave room for it. A row that can only be found by
//! its colour cannot be found under `--color never`, on a terminal that has no
//! colour, or by a reader who cannot tell two of them apart.

use std::io::{self, Write};
use std::time::Duration;

use tetanus_protocol::methods::SessionListResult;
use tetanus_protocol::types::SessionInfo;
use tetanus_ui::{bar, show, size, Flow, Frame, Key, Role, Show, Stop, Theme, Tty, Ui, View};

use super::browse::NAME;
use super::sessions;

/// How long the loop waits for a keystroke before painting again.
///
/// A listing on disk changes no faster than the reader does, and a resize
/// arrives on the same queue as the keys, so every reason to redraw ends the
/// wait early. The same figure, for the same reason, as `browse`.
const IDLE: Duration = Duration::from_secs(3600);

/// Rows a frame spends on furniture: the heading, a blank under it, a blank
/// over the footer, and the footer. Everything left is the list.
const CHROME: usize = 4;

/// Columns the cursor mark takes, on the row it is on and on every other.
const MARK: usize = 2;

/// Show `list` on the alternate screen, and say how the reader left.
///
/// An empty list never opens a screen: `tetanus sessions`' own line - that
/// nothing has been written yet, and what writes one - is the whole message,
/// and a blank page with a cursor on nothing is a worse way to say it.
pub fn pick<W: Write>(out: &mut Ui<W>, list: &SessionListResult) -> io::Result<Stop> {
    if list.sessions.is_empty() {
        sessions::render(out, list)?;
        return Ok(Stop::Quit);
    }
    let theme = *out.theme();
    let (cols, rows) = size();
    let mut picker = Picker::new(theme, list, cols);
    show(
        Tty::new(io::stdout()),
        out,
        &mut picker,
        Show {
            size: (cols, rows),
            wait: IDLE,
        },
    )
}

/// A list of journals, and a cursor on one of them.
struct Picker<'a> {
    theme: Theme,
    /// The sessions in the order they are shown, newest first.
    sessions: Vec<&'a SessionInfo>,
    /// Those sessions as rows, composed for the width below.
    rows: Vec<String>,
    /// The terminal width the rows were composed for.
    cols: usize,
    /// Which row the cursor is on.
    at: usize,
    /// The first row shown, which is what the window follows the cursor with.
    top: usize,
    /// Rows the last frame had room for, which is what a PageDown is measured
    /// in. Zero until the first frame, so no key moves further than one row
    /// before the view has been drawn once.
    room: usize,
}

impl<'a> Picker<'a> {
    /// A picker over `list`, its rows composed for a terminal `cols` wide.
    fn new(theme: Theme, list: &'a SessionListResult, cols: usize) -> Self {
        let mut picker = Self {
            theme,
            sessions: sessions::ordered(list),
            rows: Vec::new(),
            // Not `cols`: the fill below is what makes the rows true at a
            // width, and starting them equal would claim it already had.
            cols: 0,
            at: 0,
            top: 0,
            room: 0,
        };
        picker.fill(cols);
        picker
    }

    /// Compose every row again for a terminal `cols` wide.
    fn fill(&mut self, cols: usize) {
        self.rows = sessions::rows(&self.theme, cols.saturating_sub(MARK), &self.sessions);
        self.cols = cols;
    }

    /// Move the window so the cursor is on it.
    fn follow(&mut self) {
        if self.room == 0 {
            return;
        }
        if self.at < self.top {
            self.top = self.at;
        } else if self.at >= self.top + self.room {
            self.top = self.at + 1 - self.room;
        }
        self.top = self.top.min(self.rows.len().saturating_sub(self.room));
    }

    /// The keys on the left, where the cursor is on the right.
    ///
    /// Counted against the whole list rather than against the part of it on
    /// screen, because "4 of 27" is the answer to the question a reader has.
    fn footer(&self, cols: usize) -> String {
        let keys = format!(
            "{} move {} q quit",
            self.theme.glyph("↑↓", "up/dn"),
            self.theme.glyph("·", "-"),
        );
        let here = format!("{} of {}", self.at + 1, self.rows.len());
        bar(
            cols,
            &self.theme.paint(Role::Muted, &keys).to_string(),
            &self.theme.paint(Role::Accent, &here).to_string(),
        )
    }
}

impl View for Picker<'_> {
    fn frame(&mut self, cols: usize, rows: usize) -> Frame {
        if cols != self.cols {
            self.fill(cols);
        }
        self.room = rows.saturating_sub(CHROME);
        self.follow();

        let mut frame = Frame::new(cols, rows);
        frame.row(bar(
            cols,
            &self.theme.paint(Role::Heading, NAME).to_string(),
            &self.theme.paint(Role::Muted, "sessions").to_string(),
        ));
        frame.blank();
        for (index, row) in self
            .rows
            .iter()
            .enumerate()
            .skip(self.top)
            .take(self.room.max(1))
        {
            let mark = if index == self.at {
                self.theme.glyph("› ", "> ")
            } else {
                "  "
            };
            frame.row(format!("{mark}{row}"));
        }
        while frame.free() > 1 {
            frame.blank();
        }
        frame.row(self.footer(cols));
        frame
    }

    fn key(&mut self, key: Key) -> Flow {
        // One row less than a screenful, so the row the reader was looking at
        // is still on the screen they land on.
        let screenful = self.room.saturating_sub(1).max(1);
        let last = self.rows.len().saturating_sub(1);
        match key {
            Key::Char('q') | Key::Esc => return Flow::Stop,
            Key::Up => self.at = self.at.saturating_sub(1),
            Key::Down => self.at = (self.at + 1).min(last),
            Key::PageUp => self.at = self.at.saturating_sub(screenful),
            Key::PageDown => self.at = (self.at + screenful).min(last),
            Key::Home => self.at = 0,
            Key::End => self.at = last,
            _ => {}
        }
        Flow::Go
    }
}

/// Test Design Specification: the session list on a screen of its own.
///
/// Features tested: that the page holds the rows `tetanus sessions` prints, in
/// its order, with a cursor findable without colour; that the window follows
/// the cursor through a list taller than the screen and the footer counts
/// against the whole of it; the key map, including the two keys that close the
/// view; and that a resize composes the rows again at the new width.
///
/// Features NOT tested here: the wording and widths of a row (owned by
/// `sessions.rs`), the arrangement of a frame (owned by `tetanus_ui::Frame`),
/// the loop and its handling of Ctrl-C and resize (owned by
/// `tetanus_ui::show`), and the refusal of `--ui` with no terminal (owned by
/// `main.rs`, asserted end to end by TC-CLI-UI-16).
///
/// Environmental needs: none. Every case is a pure function of the sessions it
/// states and the size it asks for. No case opens a terminal.
#[cfg(test)]
mod tests {
    use tetanus_protocol::types::AgentState;
    use tetanus_ui::{buffered, Charset};

    use super::*;

    const COLS: usize = 60;

    fn theme() -> Theme {
        Theme::new(false, Charset::Unicode)
    }

    fn session(id: &str, created: u64) -> SessionInfo {
        SessionInfo {
            session_id: id.into(),
            path: format!("sessions/{id}.jsonl"),
            provider: "mock".into(),
            model: "mock-echo-1".into(),
            created_time: created,
            last_seq: 2,
            title: Some(format!("about {id}")),
            state: AgentState::Idle,
        }
    }

    /// A list of `count` sessions, oldest first, so the view has to reorder it.
    fn list(count: u64) -> SessionListResult {
        SessionListResult {
            sessions: (0..count)
                .map(|n| session(&format!("s{n}"), n + 1))
                .collect(),
        }
    }

    /// One frame, as rows of text, with the terminal's own control codes gone.
    fn rows(view: &mut Picker, cols: usize, rows: usize) -> Vec<String> {
        let frame = view.frame(cols, rows);
        let mut ui = buffered(theme(), cols);
        frame.paint(&mut ui).expect("paint");
        ui.contents()
            .trim_start_matches("\x1b[H")
            .trim_end_matches("\x1b[J")
            .split("\r\n")
            .map(|row| row.trim_end_matches("\x1b[K").trim_end().to_string())
            .collect()
    }

    /// The rows between the heading's blank and the footer, blanks dropped.
    fn body(rows: &[String]) -> Vec<String> {
        rows[2..rows.len() - 1]
            .iter()
            .filter(|row| !row.is_empty())
            .cloned()
            .collect()
    }

    /// TC-CLI-PICK-1: three sessions on a screen with room for all of them.
    /// Expected: the rows `tetanus sessions` composes, newest first, each
    /// behind two columns of cursor mark, and the newest one marked. This is
    /// the promise the view rests on - a list on a screen of its own has to be
    /// the same list, or the two ways of reading one disagree.
    #[test]
    fn what_the_page_holds_is_the_session_list() {
        let list = list(3);
        let mut view = Picker::new(theme(), &list, COLS);
        let want = sessions::rows(&theme(), COLS - MARK, &sessions::ordered(&list));

        let shown = body(&rows(&mut view, COLS, 12));
        assert_eq!(shown.len(), 3);
        assert_eq!(shown[0], format!("› {}", want[0]));
        assert_eq!(shown[1], format!("  {}", want[1]));
        assert!(
            want[0].starts_with("s2"),
            "the newest is not first: {want:?}"
        );
    }

    /// TC-CLI-PICK-2: eight sessions on a screen with room for four.
    /// Expected: the window starts at the top and moves only when the cursor
    /// would leave it; End reaches the last row and Home comes back; and the
    /// footer counts the cursor against the whole list, not against the part
    /// of it on screen.
    #[test]
    fn the_window_follows_the_cursor() {
        let list = list(8);
        let mut view = Picker::new(theme(), &list, COLS);

        let shown = rows(&mut view, COLS, 8);
        assert_eq!(body(&shown).len(), 4);
        assert!(shown[shown.len() - 1].contains("1 of 8"), "{shown:?}");

        for _ in 0..4 {
            view.key(Key::Down);
        }
        let shown = rows(&mut view, COLS, 8);
        assert!(body(&shown)[3].starts_with('›'), "{shown:?}");
        assert!(shown[shown.len() - 1].contains("5 of 8"), "{shown:?}");

        view.key(Key::End);
        let shown = rows(&mut view, COLS, 8);
        assert!(body(&shown)[3].starts_with('›'), "{shown:?}");
        assert!(shown[shown.len() - 1].contains("8 of 8"), "{shown:?}");

        view.key(Key::Home);
        let shown = rows(&mut view, COLS, 8);
        assert!(body(&shown)[0].starts_with('›'), "{shown:?}");
        assert!(shown[shown.len() - 1].contains("1 of 8"), "{shown:?}");
    }

    /// TC-CLI-PICK-3: a screenful at a time, and the keys that close the view.
    /// Expected: PageDown moves the cursor by the height less the furniture
    /// and one row kept for the reader's place, neither end overshoots, `q`
    /// and Esc stop, and a key with no meaning here is ignored. Esc is named
    /// because a key this view does not know arrives as `Esc`, so it is the
    /// one that closes a view by accident if it is not deliberate.
    #[test]
    fn the_keys_move_the_cursor_by_what_they_say() {
        let list = list(8);
        let mut view = Picker::new(theme(), &list, COLS);
        rows(&mut view, COLS, 8);

        view.key(Key::PageDown);
        assert_eq!(view.at, 3);
        view.key(Key::PageDown);
        assert_eq!(view.at, 6);
        view.key(Key::PageDown);
        assert_eq!(view.at, 7, "the cursor ran off the end");
        view.key(Key::PageUp);
        assert_eq!(view.at, 4);

        assert_eq!(view.key(Key::Char('x')), Flow::Go);
        assert_eq!(view.at, 4, "an unknown key moved the cursor");
        assert_eq!(view.key(Key::Char('q')), Flow::Stop);
        assert_eq!(view.key(Key::Esc), Flow::Stop);
    }

    /// TC-CLI-PICK-4: the terminal is made narrower while the list is open.
    /// Expected: the rows are composed again at the new width, so no row
    /// overruns the screen and the titles are cut to what is left rather than
    /// whole rows being chopped where the frame ends.
    #[test]
    fn a_resize_composes_the_rows_again() {
        let list = list(3);
        let mut view = Picker::new(theme(), &list, 70);

        assert_eq!(body(&rows(&mut view, 70, 12)).len(), 3);
        let narrow = rows(&mut view, 40, 12);
        for row in &narrow {
            assert!(row.chars().count() <= 40, "`{row}` overruns 40");
        }
        assert_eq!(
            body(&narrow)[0],
            format!(
                "› {}",
                sessions::rows(&theme(), 40 - MARK, &sessions::ordered(&list))[0]
            )
        );
    }
}
