//! The journals this build has written, as a page you pick one from.
//!
//! `tetanus sessions` prints a list whose only use is to be read an id or a
//! path out of. A directory with a hundred journals in it prints as a hundred
//! lines poured into the scrollback, and the one wanted is somewhere in the
//! middle of them. This module is the other view of the same list: the
//! alternate screen, a window on as much of it as fits, a cursor to move
//! through it with, `/` to take it down to the journals that match a word,
//! and Enter to read the journal under the cursor.
//!
//! # Stakeholders and concerns
//!
//! - *A person looking for a turn from earlier*: can I find it and read it
//!   without leaving the screen or retyping anything?
//! - *The presentation lane*: is there one composer of a session row, or two?
//! - *A reviewer of the crate seam*: what does this add to `tetanus-ui`?
//!
//! # Composition
//!
//! ```text
//! pick   ── composes the list, hands it to the loop, answers how it ended
//! Picker ── the View: the rows, a cursor, and a Journal when one is open
//! ```
//!
//! Nothing here composes a row. Every row comes from
//! [`sessions::rows`](super::sessions::rows), the ones `tetanus sessions`
//! prints, so the picker cannot disagree with the command about what it is
//! showing. Nothing here composes a journal line either: an opened journal is
//! [`Journal`](super::browse::Journal), the reader `tetanus replay --ui` uses.
//! Nothing here holds the terminal or reads a key either: [`show`] does both,
//! and this module is the view it drives.
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
//!
//! # Rationale: `/` narrows the list, it does not search the screen
//!
//! A hundred journals is the case this view exists for, and a cursor is a slow
//! way through a hundred of anything. `/` takes the list down to the journals
//! whose id or title holds what is typed, and everything else then counts
//! against what is left: the footer counts matches, PageDown is a screenful of
//! matches, and Enter opens the match under the cursor. Nothing is highlighted
//! and nothing is jumped to, because a reader who has typed a word wants the
//! other ninety-odd rows gone, not coloured.
//!
//! While the filter is being typed, every printable key belongs to it - `q`
//! included, because a view that quit on `q` in the middle of a word could not
//! be used to look for `quota`. Enter accepts the filter and hands the keys
//! back to the cursor; Esc drops it and the whole list is under the cursor
//! again.
//!
//! # Rationale: one view, not two
//!
//! Opening a journal could have been a second [`show`] over a second view, run
//! after this one returned. It is one view in two states instead, because the
//! alternate screen is then entered and left once rather than three times: a
//! nested `show` gives the terminal back and takes it again between the list
//! and the journal, which a reader sees as their own shell flashing up in the
//! middle of a keypress. The cost is that this view answers `q` in two places,
//! which is stated once in [`Picker::key`].
//!
//! # Rationale: a journal that will not open is not a failure of the picker
//!
//! Reading a file can fail, and the reader is holding the terminal when it
//! does. Ending the view to report that would throw away the list they were
//! working through for the sake of what may be the one bad journal in it. So
//! the reason goes on the footer, in the warning colour, the cursor stays where
//! it was, and the next key clears it.

use std::io::{self, Write};
use std::time::Duration;

use tetanus_protocol::methods::SessionListResult;
use tetanus_protocol::types::{SessionEvent, SessionInfo};
use tetanus_ui::{bar, show, size, Flow, Frame, Key, Role, Show, Stop, Theme, Tty, Ui, View};

use super::browse::{Exit, Journal, NAME};
use super::keys::{self, Row};
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

/// Whether the keys are moving a cursor or spelling a filter.
#[derive(Debug, PartialEq, Eq)]
enum Filter {
    /// No filter: every journal is on the list, and the keys move the cursor.
    Off,
    /// Being typed. The list narrows on each keystroke rather than on Enter,
    /// so a word is answered while it is being written and a reader can stop
    /// typing as soon as the row they want is the only one left.
    Typing(String),
    /// Accepted: the list stays narrowed, and the keys move the cursor again.
    On(String),
}

impl Filter {
    /// What has been typed, which is the empty string when nothing has.
    fn text(&self) -> &str {
        match self {
            Filter::Off => "",
            Filter::Typing(text) | Filter::On(text) => text,
        }
    }
}

/// Whether `row` is one of the sessions `wanted` names, `wanted` already
/// lowercased. Matched against the id and the title, because those are the two
/// columns a reader recognises a session by; the other two are a count and one
/// of three words, which the eye does better than a filter would.
fn held(row: &SessionInfo, wanted: &str) -> bool {
    if wanted.is_empty() {
        return true;
    }
    row.session_id.to_lowercase().contains(wanted)
        || row
            .title
            .as_deref()
            .is_some_and(|title| title.to_lowercase().contains(wanted))
}

/// Reads a journal by path, or says in one line why it could not.
///
/// Taken as a closure rather than done here so that opening a journal, and the
/// wording of every way that fails, stays in the one place the binary already
/// does both. This module knows about screens, not about files.
pub type Open<'a> = dyn Fn(&str) -> Result<Vec<SessionEvent>, String> + 'a;

/// Show `list` on the alternate screen, and say how the reader left.
///
/// An empty list never opens a screen: `tetanus sessions`' own line - that
/// nothing has been written yet, and what writes one - is the whole message,
/// and a blank page with a cursor on nothing is a worse way to say it.
pub fn pick<W: Write>(
    out: &mut Ui<W>,
    list: &SessionListResult,
    think: bool,
    open: &Open<'_>,
) -> io::Result<Stop> {
    if list.sessions.is_empty() {
        sessions::render(out, list)?;
        return Ok(Stop::Quit);
    }
    let theme = *out.theme();
    let (cols, rows) = size();
    let mut picker = Picker::new(theme, list, think, open, cols);
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

/// A list of journals, and whichever one of them the reader has opened.
struct Picker<'a> {
    theme: Theme,
    /// Every session, in the order they are shown, newest first.
    sessions: Vec<&'a SessionInfo>,
    /// The ones the filter leaves, which is what the rows are composed from,
    /// what the cursor counts against, and what Enter opens out of. All of
    /// them when there is no filter.
    shown: Vec<&'a SessionInfo>,
    /// What has been typed after `/`, and whether it is still being typed.
    filter: Filter,
    /// Those shown sessions as rows, composed for the width below.
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
    think: bool,
    open: &'a Open<'a>,
    /// The journal being read, if the reader has opened one.
    reading: Option<Journal>,
    /// Why the last journal would not open, until the next key.
    fault: Option<String>,
    /// Whether the key card is up in place of the list.
    help: bool,
}

impl<'a> Picker<'a> {
    /// A picker over `list`, its rows composed for a terminal `cols` wide.
    fn new(
        theme: Theme,
        list: &'a SessionListResult,
        think: bool,
        open: &'a Open<'a>,
        cols: usize,
    ) -> Self {
        let sessions = sessions::ordered(list);
        let mut picker = Self {
            theme,
            shown: sessions.clone(),
            sessions,
            filter: Filter::Off,
            rows: Vec::new(),
            // Not `cols`: the fill below is what makes the rows true at a
            // width, and starting them equal would claim it already had.
            cols: 0,
            at: 0,
            top: 0,
            room: 0,
            think,
            open,
            reading: None,
            fault: None,
            help: false,
        };
        picker.fill(cols);
        picker
    }

    /// Compose every shown row again for a terminal `cols` wide.
    fn fill(&mut self, cols: usize) {
        self.rows = sessions::rows(&self.theme, cols.saturating_sub(MARK), &self.shown);
        self.cols = cols;
    }

    /// Take the list down to the sessions the filter leaves, and compose them.
    fn narrow(&mut self) {
        let wanted = self.filter.text().to_lowercase();
        self.shown = self
            .sessions
            .iter()
            .copied()
            .filter(|row| held(row, &wanted))
            .collect();
        // The cursor was on a row that may not be here any more, and the
        // window it sat in was measured against a list that has changed under
        // it. Both go back to somewhere that exists.
        self.at = self.at.min(self.shown.len().saturating_sub(1));
        self.top = 0;
        self.fill(self.cols);
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

    /// The keys on the left, or the filter, or what went wrong; where the
    /// cursor is, right.
    ///
    /// Counted against the whole list rather than against the part of it on
    /// screen, because "4 of 27" is the answer to the question a reader has.
    /// Under a filter the whole list is the matches, which is what the cursor
    /// moves through and what the reader is choosing between.
    fn footer(&self, cols: usize) -> String {
        let dot = self.theme.glyph("·", "-");
        let left = match (&self.fault, &self.filter) {
            (Some(why), _) => self.theme.paint(Role::Warn, why).to_string(),
            // A filter being typed has the footer to itself: the keys it would
            // sit beside are the keys it has taken over.
            (None, Filter::Typing(text)) => self
                .theme
                .paint(Role::Accent, &format!("/{text}"))
                .to_string(),
            (None, Filter::On(text)) => format!(
                "{} {dot} {}",
                self.theme.paint(Role::Accent, &format!("/{text}")),
                self.theme.paint(Role::Muted, "esc clear"),
            ),
            (None, Filter::Off) => {
                let full = format!(
                    "{} move {dot} enter read {dot} / filter {dot} ? keys {dot} q quit",
                    self.theme.glyph("↑↓", "up/dn"),
                );
                let keys = keys::hint(cols, &full, &format!("? keys {dot} q quit"));
                self.theme.paint(Role::Muted, &keys).to_string()
            }
        };
        let here = if self.rows.is_empty() {
            // Not "1 of 0": the cursor is on nothing, and saying it is on the
            // first of nothing is a worse answer than saying there is nothing.
            "0 of 0".to_string()
        } else {
            format!("{} of {}", self.at + 1, self.rows.len())
        };
        bar(
            cols,
            &left,
            &self.theme.paint(Role::Accent, &here).to_string(),
        )
    }

    /// The list itself: a cursor on a row, and the rest of the rows around it.
    fn list(&mut self, cols: usize, rows: usize) -> Frame {
        if cols != self.cols {
            self.fill(cols);
        }
        if self.help {
            return keys::card(&self.theme, cols, rows, "session list", &self.map());
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
        if self.rows.is_empty() {
            // Only reachable under a filter, because `pick` answers an empty
            // directory before a screen is opened. A blank page reads as a
            // view that failed to draw, so the answer is written out.
            let none = format!("nothing matches {}", self.filter.text());
            frame.row(format!("  {}", self.theme.paint(Role::Muted, &none)));
        }
        while frame.free() > 1 {
            frame.blank();
        }
        frame.row(self.footer(cols));
        frame
    }

    /// Every key the list answers, in the order a reader meets them.
    fn map(&self) -> Vec<Row> {
        vec![
            (self.theme.glyph("↑ ↓", "up dn"), "one row up, one row down"),
            ("pgup pgdn", "a screenful either way"),
            ("home end", "the newest journal, the oldest"),
            ("enter", "read the journal under the cursor"),
            ("/", "a word: keep the journals holding it, as you type"),
            ("esc", "clear the filter, then close the view"),
            ("q", "close the view"),
            ("?", "this card; any key goes back"),
        ]
    }

    /// Answer a key while the list is the thing on screen.
    fn choose(&mut self, key: Key) -> Flow {
        // The card is read, not worked in: the next key takes it down, and
        // that is all it does. The rule a journal states, held to here as well.
        if self.help {
            self.help = false;
            return Flow::Go;
        }
        self.fault = None;
        if matches!(self.filter, Filter::Typing(_)) {
            return self.spell(key);
        }
        // One row less than a screenful, so the row the reader was looking at
        // is still on the screen they land on.
        let screenful = self.room.saturating_sub(1).max(1);
        let last = self.rows.len().saturating_sub(1);
        match key {
            // Esc backs out of the filter before it backs out of the view, so
            // a narrowed list is one press from being whole and two from being
            // gone. The reader who meant to leave presses it twice.
            Key::Esc if self.filter != Filter::Off => {
                self.filter = Filter::Off;
                self.narrow();
            }
            Key::Char('q') | Key::Esc => return Flow::Stop,
            // `/` on a filter that is already on re-opens it for editing
            // rather than clearing it: narrowing is usually done in two goes.
            Key::Char('/') => self.filter = Filter::Typing(self.filter.text().to_string()),
            Key::Char('?') => self.help = true,
            Key::Up => self.at = self.at.saturating_sub(1),
            Key::Down => self.at = (self.at + 1).min(last),
            Key::PageUp => self.at = self.at.saturating_sub(screenful),
            Key::PageDown => self.at = (self.at + screenful).min(last),
            Key::Home => self.at = 0,
            Key::End => self.at = last,
            Key::Enter => self.read(),
            _ => {}
        }
        Flow::Go
    }

    /// Answer a key while the filter is being typed.
    fn spell(&mut self, key: Key) -> Flow {
        // Taken by value rather than borrowed out of `self.filter`, because
        // the arms below end in a call that composes the rows again, and a
        // borrow of the filter held across that is a borrow of `self`. A
        // filter is a word long, so the copy costs nothing worth naming.
        let mut text = self.filter.text().to_string();
        match key {
            Key::Char(typed) => text.push(typed),
            Key::Backspace => {
                text.pop();
            }
            // Accepted. An empty filter is no filter, so a reader who pressed
            // `/` and thought better of it is back where they started.
            Key::Enter => {
                self.filter = if text.is_empty() {
                    Filter::Off
                } else {
                    Filter::On(text)
                };
                return Flow::Go;
            }
            Key::Esc => {
                self.filter = Filter::Off;
                self.narrow();
                return Flow::Go;
            }
            // Every other key waits. The cursor keys still mean what they mean
            // once Enter has been pressed, and answering them here as well
            // would make one press of Down do a different thing depending on
            // whether a prompt the reader has stopped looking at is open.
            _ => return Flow::Go,
        }
        self.filter = Filter::Typing(text);
        self.narrow();
        Flow::Go
    }

    /// Open the journal the cursor is on, or say why not.
    fn read(&mut self) {
        let Some(chosen) = self.shown.get(self.at) else {
            return;
        };
        match (self.open)(&chosen.path) {
            // A session with a header and nothing after it is a real thing to
            // find - `session.create` writes one - and a page holding no rows
            // does not say that, it looks like a view that failed to draw.
            Ok(events) if events.is_empty() => {
                self.fault = Some(format!("{} holds nothing to read", chosen.session_id));
            }
            Ok(events) => {
                // Headed by the id, not the path: the id is what the reader
                // picked and what every other command takes, and an absolute
                // path is mostly the part of itself that is the same for all
                // of them.
                self.reading = Some(Journal::new(
                    self.theme,
                    &chosen.session_id,
                    events,
                    self.think,
                    Exit::Back,
                    (self.cols, self.room + CHROME),
                ));
            }
            Err(why) => self.fault = Some(why),
        }
    }
}

impl View for Picker<'_> {
    fn frame(&mut self, cols: usize, rows: usize) -> Frame {
        match &mut self.reading {
            Some(journal) => journal.frame(cols, rows),
            None => self.list(cols, rows),
        }
    }

    fn key(&mut self, key: Key) -> Flow {
        let Some(journal) = &mut self.reading else {
            return self.choose(key);
        };
        // The keys that close a journal close it back to the list, not out to
        // the shell. The reader came from somewhere, and that is what "back"
        // means to them; leaving the whole view here would make Enter a key
        // worth pressing only once.
        //
        // Unless the journal has something of its own up - a search being
        // spelled, where those keys are letters, or the key card, which any
        // key takes down. This is the one place the outer view has to know
        // something about the inner one, and the alternative is a reader whose
        // list closes because they went looking for `quota`.
        if !journal.busy() && matches!(key, Key::Char('q') | Key::Esc) {
            self.reading = None;
            return Flow::Go;
        }
        journal.key(key)
    }
}

/// Test Design Specification: the session list on a screen of its own, and
/// the journal opened from it.
///
/// Features tested: that the page holds the rows `tetanus sessions` prints, in
/// its order, with a cursor findable without colour; that the window follows
/// the cursor through a list taller than the screen and the footer counts
/// against the whole of it; the key map, including the two keys that close the
/// view; that a resize composes the rows again at the new width; that Enter
/// opens the journal the cursor is on, by that session's own path and headed by
/// its id; that a journal spelling a search keeps the keys that would close it;
/// that `q` leaves a journal for the list and only then the list for the
/// shell; that a journal which will not open, or holds nothing, is reported
/// without ending the view; that `/` narrows the list to the journals that
/// match, takes the printable keys while it is being typed, and moves the
/// cursor and Enter on to the matches; and that `?` spells the list's own keys
/// out over it, that a journal showing its card keeps the keys that would
/// close it, and that a footer with no room for the long wording keeps the two
/// keys a reader cannot do without.
///
/// Features NOT tested here: the wording and widths of a row (owned by
/// `sessions.rs`), the arrangement of a frame (owned by `tetanus_ui::Frame`),
/// the reading of a journal (owned by `browse.rs`), the loop and its handling
/// of Ctrl-C and resize (owned by `tetanus_ui::show`), and the refusal of
/// `--ui` with no terminal (owned by `main.rs`, asserted end to end by
/// TC-CLI-UI-16).
///
/// Environmental needs: none. The journal reader is a closure each case states,
/// so no case touches the filesystem, and no case opens a terminal.
#[cfg(test)]
mod tests {
    use serde_json::json;
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

    /// One event, so a journal that opens has something on its page.
    fn journal() -> Vec<SessionEvent> {
        vec![SessionEvent {
            ty: "user/message".into(),
            seq: 0,
            time: 0,
            data: json!({ "content": "echo this" }),
            source_event_seqs: None,
        }]
    }

    /// One frame, as rows of text, with the terminal's own control codes gone.
    fn rows<V: View>(view: &mut V, cols: usize, rows: usize) -> Vec<String> {
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

    /// Open the filter prompt and type `text` into it, one key at a time.
    fn typed(view: &mut Picker<'_>, text: &str) {
        view.key(Key::Char('/'));
        for letter in text.chars() {
            view.key(Key::Char(letter));
        }
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
        let open = |_: &str| Ok(journal());
        let mut view = Picker::new(theme(), &list, false, &open, COLS);
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
        let open = |_: &str| Ok(journal());
        let mut view = Picker::new(theme(), &list, false, &open, COLS);

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
        let open = |_: &str| Ok(journal());
        let mut view = Picker::new(theme(), &list, false, &open, COLS);
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
        let open = |_: &str| Ok(journal());
        let mut view = Picker::new(theme(), &list, false, &open, 70);

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

    /// TC-CLI-PICK-5: Enter on the second row.
    /// Expected: the page becomes that journal, headed by its id, and the path
    /// the reader is handed is the one that session names - a picker that
    /// opened the wrong file would still look like it had worked.
    #[test]
    fn enter_opens_the_journal_the_cursor_is_on() {
        let list = list(3);
        let asked = std::cell::RefCell::new(Vec::new());
        let open = |path: &str| {
            asked.borrow_mut().push(path.to_string());
            Ok(journal())
        };
        let mut view = Picker::new(theme(), &list, false, &open, COLS);
        rows(&mut view, COLS, 12);

        view.key(Key::Down);
        view.key(Key::Enter);

        assert_eq!(asked.borrow().as_slice(), ["sessions/s1.jsonl"]);
        let shown = rows(&mut view, COLS, 12);
        assert!(
            shown[0].contains("s1"),
            "the journal is not headed: {shown:?}"
        );
        assert!(
            body(&shown).iter().any(|row| row.contains("echo this")),
            "the journal is not on the page: {shown:?}"
        );
    }

    /// TC-CLI-PICK-6: `q` inside a journal, then `q` on the list.
    /// Expected: the first goes back to the list with the cursor where it was
    /// left, and only the second ends the view. Esc does the same, because a
    /// key this view does not name arrives as Esc and must not close a journal
    /// and the list with one press.
    #[test]
    fn q_leaves_a_journal_for_the_list_and_the_list_for_the_shell() {
        let list = list(3);
        let open = |_: &str| Ok(journal());
        let mut view = Picker::new(theme(), &list, false, &open, COLS);
        rows(&mut view, COLS, 12);
        view.key(Key::Down);
        view.key(Key::Enter);

        assert_eq!(view.key(Key::Esc), Flow::Go);
        let shown = rows(&mut view, COLS, 12);
        assert!(body(&shown)[1].starts_with('›'), "{shown:?}");
        assert_eq!(view.key(Key::Char('q')), Flow::Stop);
    }

    /// TC-CLI-PICK-7: a journal that will not open, and one that is empty.
    /// Expected: the reason is on the footer, the view carries on, the cursor
    /// has not moved, and the list is still under it. A reader who hit one bad
    /// journal keeps the list they were working through.
    #[test]
    fn a_journal_that_will_not_open_is_said_on_the_footer() {
        let list = list(3);
        let open = |path: &str| {
            if path.ends_with("s2.jsonl") {
                Err("s2.jsonl: line 3 is not a journal line".into())
            } else {
                Ok(Vec::new())
            }
        };
        let mut view = Picker::new(theme(), &list, false, &open, COLS);
        rows(&mut view, COLS, 12);

        view.key(Key::Enter);
        let shown = rows(&mut view, COLS, 12);
        assert!(shown[shown.len() - 1].contains("line 3"), "{shown:?}");
        assert!(body(&shown)[0].starts_with('›'), "{shown:?}");

        view.key(Key::Down);
        view.key(Key::Enter);
        let shown = rows(&mut view, COLS, 12);
        assert!(
            shown[shown.len() - 1].contains("holds nothing to read"),
            "{shown:?}"
        );
        assert_eq!(body(&shown).len(), 3, "the list went away: {shown:?}");
    }
    /// TC-CLI-PICK-8: `/` and a word, on a list of eight.
    /// Expected: the list is down to the journals that match on the keystroke
    /// which made them match, not on Enter; the match is not case sensitive,
    /// because a reader typing a filter is reading rather than retyping an id;
    /// the title matches as well as the id; and the footer shows what has been
    /// typed and counts the matches.
    #[test]
    fn a_filter_narrows_the_list_as_it_is_typed() {
        let list = list(8);
        let open = |_: &str| Ok(journal());
        let mut view = Picker::new(theme(), &list, false, &open, COLS);
        rows(&mut view, COLS, 12);

        typed(&mut view, "S3");
        let shown = rows(&mut view, COLS, 12);
        assert_eq!(body(&shown).len(), 1, "{shown:?}");
        assert!(body(&shown)[0].contains("s3"), "{shown:?}");
        assert!(shown[shown.len() - 1].contains("/S3"), "{shown:?}");
        assert!(shown[shown.len() - 1].contains("1 of 1"), "{shown:?}");

        // Every title here is "about sN", so a word out of one of them holds
        // the whole list back: the title is matched, not only the id.
        view.key(Key::Backspace);
        view.key(Key::Backspace);
        for letter in "ABOUT".chars() {
            view.key(Key::Char(letter));
        }
        let shown = rows(&mut view, COLS, 12);
        assert_eq!(body(&shown).len(), 8, "{shown:?}");
        assert!(shown[shown.len() - 1].contains("1 of 8"), "{shown:?}");
    }

    /// TC-CLI-PICK-9: the keys while the filter prompt is open.
    /// Expected: `q` is a letter and not the quit key - a view that quit here
    /// could not be used to look for `quota`; Backspace takes one letter back;
    /// Enter accepts and hands the keys back with the list still narrow; Esc
    /// drops the filter and the whole list is under the cursor again; and only
    /// then does `q` mean quit.
    #[test]
    fn the_prompt_takes_the_printable_keys_while_it_is_open() {
        let list = list(8);
        let open = |_: &str| Ok(journal());
        let mut view = Picker::new(theme(), &list, false, &open, COLS);
        rows(&mut view, COLS, 12);

        view.key(Key::Char('/'));
        assert_eq!(view.key(Key::Char('q')), Flow::Go, "`q` closed the view");
        assert_eq!(view.filter, Filter::Typing("q".into()));

        view.key(Key::Backspace);
        for letter in "s3".chars() {
            view.key(Key::Char(letter));
        }
        assert_eq!(view.key(Key::Enter), Flow::Go);
        assert_eq!(view.filter, Filter::On("s3".into()));
        assert_eq!(body(&rows(&mut view, COLS, 12)).len(), 1);

        assert_eq!(view.key(Key::Esc), Flow::Go, "Esc closed the view");
        assert_eq!(view.filter, Filter::Off);
        assert_eq!(body(&rows(&mut view, COLS, 12)).len(), 8);
        assert_eq!(view.key(Key::Char('q')), Flow::Stop);
    }

    /// TC-CLI-PICK-10: the cursor on the last of eight, then a filter leaving
    /// one; then a filter leaving none.
    /// Expected: the cursor comes back to a row that exists, and Enter opens
    /// that row's own journal rather than the eighth of the whole list - a
    /// picker that filtered the screen but not the choosing would open the
    /// wrong file and still look like it had worked. A filter that matches
    /// nothing says so on the page, and Enter on it opens nothing and does not
    /// end the view.
    #[test]
    fn the_cursor_and_enter_follow_the_matches() {
        let list = list(8);
        let asked = std::cell::RefCell::new(Vec::new());
        let open = |path: &str| {
            asked.borrow_mut().push(path.to_string());
            Ok(journal())
        };
        let mut view = Picker::new(theme(), &list, false, &open, COLS);
        rows(&mut view, COLS, 12);

        view.key(Key::End);
        assert_eq!(view.at, 7);
        typed(&mut view, "s3");
        assert_eq!(view.at, 0, "the cursor stayed off the end of the matches");

        view.key(Key::Enter);
        view.key(Key::Enter);
        assert_eq!(asked.borrow().as_slice(), ["sessions/s3.jsonl"]);
        assert_eq!(view.key(Key::Esc), Flow::Go, "the journal did not close");

        typed(&mut view, "zz");
        let shown = rows(&mut view, COLS, 12);
        assert!(body(&shown)[0].contains("nothing matches"), "{shown:?}");
        assert!(shown[shown.len() - 1].contains("0 of 0"), "{shown:?}");
        view.key(Key::Enter);
        assert_eq!(view.key(Key::Enter), Flow::Go, "Enter on no rows stopped");
        assert_eq!(asked.borrow().len(), 1, "a journal opened with no row");
    }
    /// TC-CLI-PICK-11: `/` inside an opened journal, then `q`.
    /// Expected: the journal keeps the key, because it is spelling a word with
    /// a `q` in it, and the list is still behind it. The picker is the only
    /// place two views share one keyboard, so this is the case that holds them
    /// apart.
    #[test]
    fn a_journal_spelling_a_search_keeps_the_keys_that_would_close_it() {
        let list = list(3);
        let open = |_: &str| Ok(journal());
        let mut view = Picker::new(theme(), &list, false, &open, COLS);
        rows(&mut view, COLS, 12);
        view.key(Key::Enter);

        view.key(Key::Char('/'));
        assert_eq!(view.key(Key::Char('q')), Flow::Go, "the journal closed");
        assert!(view.reading.is_some(), "the journal closed");

        // Esc closes the prompt, and the next Esc is the journal's own again.
        view.key(Key::Esc);
        assert_eq!(view.key(Key::Esc), Flow::Go);
        assert!(view.reading.is_none(), "the journal did not close");
    }

    /// TC-CLI-PICK-12: `?` over the list, and the way back out of it.
    /// Expected: the card names this view rather than a journal, holds a row
    /// for every key the list answers, and the next key - any key - gives back
    /// the list as it was, cursor where it was left.
    #[test]
    fn the_card_says_the_lists_own_keys() {
        let list = list(3);
        let open = |_: &str| Ok(journal());
        let mut view = Picker::new(theme(), &list, false, &open, COLS);
        view.key(Key::Down);
        let before = rows(&mut view, COLS, 12);

        assert_eq!(view.key(Key::Char('?')), Flow::Go);
        let card = rows(&mut view, COLS, 12);
        let shown = body(&card).join("\n");
        assert!(
            card[0].contains("session list keys"),
            "not headed as this view's card: {card:?}"
        );
        for said in [
            "one row up",
            "read the journal under the cursor",
            "clear the filter",
            "this card",
        ] {
            assert!(
                shown.contains(said),
                "the card does not say {said}: {card:?}"
            );
        }
        assert!(!shown.contains("s2"), "the list is still on it: {card:?}");

        assert_eq!(view.key(Key::Char('z')), Flow::Go);
        assert_eq!(
            rows(&mut view, COLS, 12),
            before,
            "the card moved the cursor"
        );
    }

    /// TC-CLI-PICK-13: `?` inside an opened journal, then `q`.
    /// Expected: the journal keeps both keys - the one that opens its card and
    /// the one that takes the card down - and the list is still behind it. The
    /// other half of TC-CLI-PICK-11: an inner view showing something of its
    /// own owns the keyboard until it is done with it.
    #[test]
    fn a_journal_showing_its_card_keeps_the_keys_that_would_close_it() {
        let list = list(3);
        let open = |_: &str| Ok(journal());
        let mut view = Picker::new(theme(), &list, false, &open, COLS);
        rows(&mut view, COLS, 12);
        view.key(Key::Enter);

        view.key(Key::Char('?'));
        let card = rows(&mut view, COLS, 12);
        assert!(
            card[0].contains("journal keys"),
            "the journal's card is not up: {card:?}"
        );

        // `q` takes the card down and nothing else: the journal is still open,
        // and so is the list under it.
        assert_eq!(view.key(Key::Char('q')), Flow::Go, "the journal closed");
        assert!(view.reading.is_some(), "the journal closed");
        let back = rows(&mut view, COLS, 12);
        assert!(
            !back[0].contains("journal keys"),
            "the card stayed up: {back:?}"
        );

        assert_eq!(view.key(Key::Char('q')), Flow::Go);
        assert!(view.reading.is_none(), "the journal did not close");
    }

    /// TC-CLI-PICK-14: the list's footer on a terminal too narrow for its keys.
    /// Expected: at 80 columns the whole key list is on it; at 34 the card and
    /// the way out are, and nothing else claims room it does not have.
    #[test]
    fn a_narrow_footer_keeps_the_card_and_the_way_out() {
        let list = list(3);
        let open = |_: &str| Ok(journal());
        let mut view = Picker::new(theme(), &list, false, &open, COLS);

        let wide = rows(&mut view, 80, 12);
        let footer = wide[wide.len() - 1].clone();
        assert!(
            footer.contains("enter read"),
            "the wide footer lost the keys: {footer}"
        );
        assert!(
            footer.contains("? keys"),
            "the wide footer never says `?`: {footer}"
        );

        let narrow = rows(&mut view, 34, 12);
        let footer = narrow[narrow.len() - 1].clone();
        assert!(
            footer.contains("? keys"),
            "the narrow footer lost the card: {footer}"
        );
        assert!(
            footer.contains("q quit"),
            "the narrow footer lost the way out: {footer}"
        );
        assert!(
            !footer.contains("filter"),
            "the narrow footer kept the long wording: {footer}"
        );
    }
}
