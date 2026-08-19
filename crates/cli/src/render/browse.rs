//! A finished journal, read back on a screen of its own.
//!
//! `tetanus replay <path>` prints a whole turn into the scrollback, which is
//! the right answer for a short journal and the wrong one for a long. A turn
//! with sixty tool calls in it arrives as a wall the reader then scrolls their
//! shell back through, mixed in with whatever was on the screen before. This
//! module is the other view of the same lines: the alternate screen, one page
//! at a time, with keys to move through it, `/` to reach the line that holds a
//! word, and the scrollback left exactly as it was found.
//!
//! # Stakeholders and concerns
//!
//! - *A person reading a long turn back*: can I reach the part I want, and is
//!   what I read the same text `replay` would have printed?
//! - *The presentation lane*: is there one composer of a turn's lines, or two?
//! - *A reviewer of the crate seam*: what does this add to `tetanus-ui`?
//!
//! # Composition
//!
//! ```text
//! browse  ── fills a page, hands it to the loop, answers how it ended
//! Journal ── the View: a Page over timeline::Reader's lines, and a key map
//! ```
//!
//! Nothing here composes a line. Every line on the page comes from
//! [`Reader`](super::timeline::Reader), the one `replay` already prints
//! through, so a turn read full-screen and the same turn printed are the same
//! words. Nothing here holds the terminal or reads a key either: [`show`] does
//! both, and this module is the view it drives.
//!
//! # Rationale: the first caller of `tetanus_ui::show`
//!
//! A journal is finished before the first frame is drawn. `show` waits for a
//! keystroke on the thread it was called on, which is why `run --ui` cannot use
//! it - a turn in flight has to be driven while nobody is typing, so that view
//! runs its own loop under `tokio::select!`. Reading a file back has nothing to
//! drive. So the loop the crate already ships is exactly the loop this needs,
//! and this view is a key map rather than a second driver.
//!
//! # Rationale: `/` moves the window, it does not narrow the page
//!
//! The session picker's `/` takes rows off the list, because there a row is
//! the whole answer. A line of a turn is not: "ok" means nothing without the
//! tool call over it, and a transcript cut down to the lines holding a word is
//! a grep of a conversation rather than a reading of one. So here `/` moves
//! the window to the first line that holds what was typed and leaves the turn
//! around it intact, `n` walks the rest, and the footer says which match the
//! reader is on.
//!
//! The line landed on goes to the top of the window rather than the bottom,
//! because what a reader wants after finding a line is the lines under it.
//!
//! The word itself is marked on every line drawing it, not only on the one the
//! window landed on. Moving the window says which line; on the long lines a
//! turn writes it does not say where, and the lines around a match are the
//! reason the window did not narrow to it in the first place. Marking only the
//! match the reader is on would undo that: the eye would be told where to look
//! on one line and left to search the rest.
//!
//! Nothing moves until Enter. A page that jumped on every keystroke would take
//! the reader off the line they were reading before they had finished saying
//! where they wanted to go, which is the opposite of what the picker wants and
//! for the same reason: there, what is under the cursor is the answer being
//! narrowed; here, it is the place being left.
//!
//! # Rationale: a resize refills the page
//!
//! A [`Page`] never rewraps a line it has been given, because a live view must
//! not rewrite history under a reader who is still reading it. A finished
//! journal is not under that constraint: its lines can simply be composed
//! again at the new width, and not doing so would leave a page the reader
//! widened still wrapped for the screen they widened it from. So a resize
//! refills, keeping how far back the reader had scrolled. That distance is in
//! rows and rows mean something slightly different after a rewrap, which is
//! the price of the alternative being visibly wrong. A search open at the time
//! keeps its match instead of that distance, because the line the reader was
//! taken to is the thing they were looking at, and a line can be found again
//! exactly where a row count cannot.

use std::io::{self, Write};
use std::time::Duration;

use tetanus_protocol::types::SessionEvent;
use tetanus_ui::{
    light, plain, show, size, Flow, Frame, Key, Page, Role, Show, Stop, Theme, Tty, Ui, View,
};

use super::keys::{self, Row};
use super::timeline::Reader;

/// How long the loop waits for a keystroke before painting again.
///
/// Nothing on this page changes on its own, and a resize arrives on the same
/// queue as the keys, so every reason to redraw ends the wait early anyway. A
/// short wait would buy nothing and repaint a still page ten times a second.
const IDLE: Duration = Duration::from_secs(3600);

/// Rows a PageUp keeps: the four the page spends on furniture, and one line of
/// what was on screen, so the reader does not lose their place between screens.
pub(super) const KEPT: usize = 5;

/// The left of the heading, so a screen with nothing else on it says what it is.
pub(super) const NAME: &str = "tetanus";

/// Read `events` back on the alternate screen, and say how the reader left.
///
/// An empty journal never opens a screen. `replay`'s own line - that the
/// journal is there and holds nothing - is the whole message, and a blank page
/// the reader has to press `q` to get out of is a worse way to say it.
pub fn browse<W: Write>(
    out: &mut Ui<W>,
    title: &str,
    events: &[SessionEvent],
    think: bool,
) -> io::Result<Stop> {
    if events.is_empty() {
        super::timeline::render(out, events, think)?;
        return Ok(Stop::Quit);
    }
    let theme = *out.theme();
    let (cols, rows) = size();
    let mut journal = Journal::new(
        theme,
        title,
        events.to_vec(),
        think,
        Exit::Quit,
        (cols, rows),
    );
    show(
        Tty::new(io::stdout()),
        out,
        &mut journal,
        Show {
            size: (cols, rows),
            wait: IDLE,
        },
    )
}

/// Where `q` takes a reader out of a journal.
///
/// Given by whoever opened it, because only they know: the same view is the
/// whole of `tetanus replay --ui` and one layer of `tetanus sessions --ui`,
/// and "quit" to a reader who came from the shell is "back" to a reader who
/// came from the list. Two words rather than a whole footer, so the wording
/// of a key map is written once here instead of once per caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Exit {
    /// Out to the shell: this journal is the view.
    Quit,
    /// Back to the list this journal was opened from.
    Back,
}

impl Exit {
    /// The word after `q` on the footer.
    fn word(self) -> &'static str {
        match self {
            Exit::Quit => "quit",
            Exit::Back => "back",
        }
    }

    /// The same thing said in full, for the key card.
    fn what(self) -> &'static str {
        match self {
            Exit::Quit => "close the view",
            Exit::Back => "back to the session list",
        }
    }
}

/// Whether the keys are moving the window or spelling a search.
enum Find {
    /// No search: the keys move the window.
    Off,
    /// Being typed, and nothing has moved yet.
    Typing(String),
    /// Done. `hits` are the lines holding `text`, oldest first, and `at` is
    /// the one the window was put on. An empty `hits` is an answer, not a
    /// state to leave: the reader is told, and the page has not moved.
    On {
        text: String,
        hits: Vec<usize>,
        at: usize,
    },
}

/// A journal on a page, and the keys that move through it.
///
/// It owns its events rather than borrowing them because a surface that picks
/// a journal loads one while the view is already running: a borrow would have
/// to come from something outliving the view, and there is nothing there to
/// hold it.
pub(super) struct Journal {
    /// Kept, not just read once, because a resize composes them again.
    events: Vec<SessionEvent>,
    theme: Theme,
    think: bool,
    title: String,
    page: Page,
    /// Where `q` goes from here, which is the one thing about this view its
    /// caller knows and it does not.
    exit: Exit,
    /// Whether the key card is up in place of the transcript.
    help: bool,
    /// The width the page was last filled at.
    cols: usize,
    /// The height of the last frame, which is what a PageUp is measured in.
    rows: usize,
    /// The visible text of every settled line, in the page's own order, which
    /// is what a search is made against. Kept beside the page rather than
    /// asked of it, because a painted line holds the codes the theme wrote
    /// between its words and a reader looking for two of them either side of a
    /// colour change would not find them.
    plain: Vec<String>,
    find: Find,
}

impl Journal {
    /// A journal filled and ready to draw at `size`.
    pub(super) fn new(
        theme: Theme,
        title: &str,
        events: Vec<SessionEvent>,
        think: bool,
        exit: Exit,
        size: (usize, usize),
    ) -> Self {
        let mut journal = Self {
            events,
            theme,
            think,
            title: title.to_string(),
            page: Page::new(theme, NAME, title),
            exit,
            help: false,
            // Not `size.0`: the fill below is what makes the page true at a
            // width, and starting them equal would claim it already had.
            cols: 0,
            rows: size.1,
            plain: Vec::new(),
            find: Find::Off,
        };
        journal.fill(size.0);
        journal
    }

    /// Compose every line again at `cols`, keeping how far back the reader is.
    ///
    /// This is also where a search is marked, because the mark goes into the
    /// line itself: the page is handed finished strings and does not know a
    /// search happened. So every way the mark can change - the word, the
    /// width, or there being one at all - comes back through here.
    fn fill(&mut self, cols: usize) {
        let back = self.page.back();
        // Cloned rather than borrowed, because the loop below borrows the rest
        // of `self` to compose with. A search is a word long.
        let wanted = self.wanted();
        let mut page = Page::new(self.theme, NAME, &self.title);
        let mut reader = Reader::new(self.think);
        let mut plains = Vec::new();
        for event in &self.events {
            let lines = reader.lines(&self.theme, cols, event);
            for line in &lines {
                plains.push(plain(line));
            }
            // Marked after the plain text is taken, which is what the search
            // is made against: the mark is escapes only, so it would not
            // change the answer, but it would be work done to be undone.
            page.settle(match &wanted {
                Some(word) => lines.iter().map(|line| light(line, word)).collect(),
                None => lines,
            });
        }
        page.scroll(isize::try_from(back).unwrap_or(isize::MAX));
        self.page = page;
        self.plain = plains;
        self.cols = cols;
        // A rewrap moves every line, so the lines a search found are not the
        // lines holding it any more. Found again rather than dropped, and
        // landed on again: a reader who widened the terminal did not ask to
        // lose their search, and the match is what they were looking at.
        self.seek();
        self.land();
    }

    /// The word to mark on every line holding it, if there is one to mark.
    ///
    /// A search still being spelled is not one: marking it would mean
    /// composing the whole journal again on every keystroke, and the reader
    /// spelling a word is looking at the prompt they are spelling it into.
    ///
    /// With colour off there is nothing to mark with. A plain theme adds no
    /// bytes anywhere else either, and a page that answered `--color never`
    /// with escapes would be the one place the flag did not hold.
    fn wanted(&self) -> Option<String> {
        match &self.find {
            Find::On { text, .. } if self.theme.color() => Some(text.clone()),
            _ => None,
        }
    }

    /// Find the lines holding the current search, and keep the reader on the
    /// same match number where there is still one there.
    fn seek(&mut self) {
        let Find::On { text, hits, at } = &mut self.find else {
            return;
        };
        let wanted = text.to_lowercase();
        *hits = self
            .plain
            .iter()
            .enumerate()
            .filter(|(_, line)| line.to_lowercase().contains(&wanted))
            .map(|(line, _)| line)
            .collect();
        *at = (*at).min(hits.len().saturating_sub(1));
    }

    /// Search for `text`, and go to the first line holding it.
    fn accept(&mut self, text: String) {
        self.find = Find::On {
            text,
            hits: Vec::new(),
            at: 0,
        };
        // Filled again rather than sought: the word has to be marked on the
        // lines, and `fill` seeks and lands at the end of it.
        self.fill(self.cols);
    }

    /// Go to the next match, or the one before, wrapping at either end.
    fn walk(&mut self, forward: bool) {
        let Find::On { hits, at, .. } = &mut self.find else {
            return;
        };
        if hits.is_empty() {
            return;
        }
        // Round rather than stopping: a reader who reaches the last match and
        // presses `n` again is asking to go round, not to be told they cannot.
        *at = if forward {
            (*at + 1) % hits.len()
        } else {
            (*at + hits.len() - 1) % hits.len()
        };
        self.land();
    }

    /// Put the window on the match the reader is on, that line at the top.
    fn land(&mut self) {
        let Find::On { hits, at, .. } = &self.find else {
            return;
        };
        let Some(line) = hits.get(*at).copied() else {
            return;
        };
        // The body is the frame less its furniture, which is `KEPT` less the
        // one line of their place a PageUp holds on to for the reader.
        let room = self.rows.saturating_sub(KEPT - 1).max(1);
        let back = self.plain.len().saturating_sub(room + line);
        self.page.follow();
        self.page
            .scroll(isize::try_from(back).unwrap_or(isize::MAX));
    }

    /// The left of the footer: the caller's keys, or the search over them.
    ///
    /// `Page` paints whatever it is handed in the muted role, and a span
    /// painted here keeps its own colour inside that, which is how the prompt
    /// stays the one thing on the footer the eye lands on.
    fn hint(&self) -> String {
        let dot = self.theme.glyph("·", "-");
        match &self.find {
            Find::Off => self.reading(),
            Find::Typing(text) => self
                .theme
                .paint(Role::Accent, &format!("/{text}"))
                .to_string(),
            Find::On { text, hits, .. } if hits.is_empty() => self
                .theme
                .paint(Role::Warn, &format!("no line holds {text}"))
                .to_string(),
            Find::On { text, hits, at } => {
                let count = format!("match {} of {} {dot} n next", at + 1, hits.len());
                format!(
                    "{} {dot} {}",
                    self.theme.paint(Role::Accent, &format!("/{text}")),
                    self.theme.paint(Role::Muted, &count),
                )
            }
        }
    }

    /// The footer while the reader is reading rather than searching, in the
    /// longest wording this terminal has room for.
    ///
    /// The short one names the two keys a reader cannot do without: the card
    /// that says the rest, and the way out. Everything it drops is on the
    /// card, which is what lets it be dropped.
    fn reading(&self) -> String {
        let dot = self.theme.glyph("·", "-");
        let out = self.exit.word();
        keys::hint(
            self.cols,
            &format!(
                "{} scroll {dot} / find {dot} ? keys {dot} q {out}",
                self.theme.glyph("↑↓", "up/dn")
            ),
            &format!("? keys {dot} q {out}"),
        )
    }

    /// Every key this view answers, in the order a reader meets them.
    fn map(&self) -> Vec<Row> {
        vec![
            (
                self.theme.glyph("↑ ↓", "up dn"),
                "one line back, one line on",
            ),
            ("pgup pgdn", "a screenful either way"),
            ("home end", "the first line of the turn, the last"),
            ("/", "a word, then enter: go to the line holding it"),
            ("n N", "the next line holding it, the one before"),
            ("esc", "while spelling: drop the word, move nothing"),
            ("q", self.exit.what()),
            ("?", "this card; any key goes back"),
        ]
    }

    /// Whether a word is being spelled into this journal's search prompt.
    fn typing(&self) -> bool {
        matches!(self.find, Find::Typing(_))
    }

    /// Whether this journal has something of its own over the transcript: a
    /// word being spelled, or the key card.
    ///
    /// Asked by a surface that opened this journal inside a view of its own.
    /// While either is up the keys that would close a journal belong to it -
    /// `q` is a letter of a search, and the card is closed by any key at all -
    /// so its owner has to know not to answer them.
    pub(super) fn busy(&self) -> bool {
        self.typing() || self.help
    }

    /// Answer a key while the search prompt is open.
    fn spell(&mut self, key: Key) -> Flow {
        // Copied out rather than borrowed, because the arms below call back
        // into `self`. A search is a word long, so the copy costs nothing.
        let Find::Typing(current) = &self.find else {
            return Flow::Go;
        };
        let mut text = current.clone();
        match key {
            Key::Char(typed) => text.push(typed),
            Key::Backspace => {
                text.pop();
            }
            Key::Enter if !text.is_empty() => {
                self.accept(text);
                return Flow::Go;
            }
            // Enter on an empty prompt, and Esc, both hand the keys back
            // without moving the page.
            Key::Enter | Key::Esc => {
                self.find = Find::Off;
                self.fill(self.cols);
                return Flow::Go;
            }
            _ => return Flow::Go,
        }
        self.find = Find::Typing(text);
        Flow::Go
    }
}

impl View for Journal {
    fn frame(&mut self, cols: usize, rows: usize) -> Frame {
        // Set before the refill below, because a search that lands again
        // after a rewrap measures the window it lands in with it.
        self.rows = rows;
        if cols != self.cols {
            self.fill(cols);
        }
        if self.help {
            return keys::card(&self.theme, cols, rows, "journal", &self.map());
        }
        let keys = self.hint();
        // No block: a journal on disk has nothing arriving, which is what the
        // footer reads back as `end` rather than `live`.
        self.page.frame(cols, rows, &[], &keys)
    }

    fn key(&mut self, key: Key) -> Flow {
        if self.typing() {
            return self.spell(key);
        }
        // The card is read, not worked in. Whatever is pressed next takes it
        // down and does nothing else, so a reader who opened it by accident is
        // one key from where they were, whichever key they try.
        if self.help {
            self.help = false;
            return Flow::Go;
        }
        let screenful = isize::try_from(self.rows.saturating_sub(KEPT).max(1)).unwrap_or(1);
        let found = matches!(self.find, Find::On { .. });
        match key {
            Key::Char('q') | Key::Esc => return Flow::Stop,
            // The marks of the search being replaced come off now rather than
            // when the new word is accepted: a word being spelled under the
            // marks of the last one reads as a search that has already run.
            Key::Char('/') => {
                self.find = Find::Typing(String::new());
                self.fill(self.cols);
            }
            Key::Char('?') => self.help = true,
            // `n` and `N` mean nothing until a search has been made. A letter
            // that moved the page before then would be a trap in a view whose
            // whole content is letters.
            Key::Char('n') if found => self.walk(true),
            Key::Char('N') if found => self.walk(false),
            Key::Up => self.page.scroll(1),
            Key::Down => self.page.scroll(-1),
            Key::PageUp => self.page.scroll(screenful),
            Key::PageDown => self.page.scroll(-screenful),
            // The far end is clamped against the transcript, so asking for
            // every row there is is how you ask for the first line of it.
            Key::Home => self.page.scroll(isize::MAX),
            Key::End => self.page.follow(),
            _ => {}
        }
        Flow::Go
    }
}

/// Test Design Specification: a journal read back full-screen.
///
/// Features tested: that the page holds the timeline's own lines and no
/// others; that a journal taller than the screen shows its tail and can be
/// walked back to its first line; that a resize composes the lines again at
/// the new width without losing the reader's place; the key map, including the
/// two keys that end the view; that `/` reaches the line holding a word, that
/// `n` walks the rest of them, that the prompt takes the printable keys while
/// it is open, that a word no line holds is said rather than acted on, and
/// that the word found is marked wherever it is drawn and only on a terminal
/// that was given colour; and
/// that `?` spells the keys out over the transcript, that any key takes the
/// card down again, and that a footer with no room for the long wording keeps
/// the two keys a reader cannot do without.
///
/// Features NOT tested here: the wording of a line (owned by `timeline.rs`),
/// the arrangement of a frame (owned by `tetanus_ui::Page`), the loop and its
/// handling of Ctrl-C and resize (owned by `tetanus_ui::show`), and the
/// refusal of `--ui` with no terminal (owned by `main.rs`, asserted end to end
/// by TC-CLI-UI-15).
///
/// Environmental needs: none. Every case is a pure function of the events it
/// states and the size it asks for. No case opens a terminal.
#[cfg(test)]
mod tests {
    use serde_json::json;
    use tetanus_ui::{buffered, Charset};

    use super::*;

    const COLS: usize = 60;

    fn event(ty: &str, data: serde_json::Value) -> SessionEvent {
        SessionEvent {
            ty: ty.into(),
            seq: 0,
            time: 0,
            data,
            source_event_seqs: None,
        }
    }

    fn theme() -> Theme {
        Theme::new(false, Charset::Unicode)
    }

    fn turn() -> Vec<SessionEvent> {
        vec![
            event("session/start", json!({ "model": "deepseek-chat" })),
            event("turn/start", json!({ "turn": 1 })),
            event("step/start", json!({ "turn": 1, "step": 1 })),
            event("user/message", json!({ "content": "echo this" })),
            event("assistant/message", json!({ "content": "on it" })),
            event(
                "tool/call",
                json!({ "id": "c1", "name": "echo", "arguments": { "text": "hi" } }),
            ),
            event(
                "tool/result",
                json!({ "call_id": "c1", "name": "echo", "ok": true, "content": "hi" }),
            ),
            event("step/end", json!({ "turn": 1, "step": 1 })),
            event(
                "turn/end",
                json!({ "turn": 1, "steps": 1, "stop_reason": "natural" }),
            ),
        ]
    }

    /// The lines `replay` would have printed for the same events.
    fn timeline(events: &[SessionEvent], cols: usize) -> Vec<String> {
        let mut ui = buffered(theme(), cols);
        super::super::timeline::render(&mut ui, events, false).expect("render");
        ui.contents().lines().map(str::to_string).collect()
    }

    /// The same journal on a terminal that has colour, which is the only kind
    /// a mark is written for.
    fn painted(events: &[SessionEvent], cols: usize) -> Journal {
        Journal::new(
            Theme::new(true, Charset::Unicode),
            "j.jsonl",
            events.to_vec(),
            false,
            Exit::Quit,
            (cols, 0),
        )
    }

    fn journal(events: &[SessionEvent], cols: usize) -> Journal {
        Journal::new(
            theme(),
            "j.jsonl",
            events.to_vec(),
            false,
            Exit::Quit,
            (cols, 0),
        )
    }

    /// One frame, as rows of text, with the terminal's own control codes gone.
    fn rows(view: &mut Journal, cols: usize, rows: usize) -> Vec<String> {
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

    /// Open the search prompt and type `text` into it, one key at a time.
    fn typed(view: &mut Journal, text: &str) {
        view.key(Key::Char('/'));
        for letter in text.chars() {
            view.key(Key::Char(letter));
        }
    }

    /// The body of a frame: everything between the heading's blank and the
    /// footer, with the padding a short transcript leaves under it dropped.
    fn body(rows: &[String]) -> Vec<String> {
        let body = &rows[2..rows.len() - 1];
        let end = body
            .iter()
            .rposition(|row| !row.is_empty())
            .map_or(0, |i| i + 1);
        body[..end].to_vec()
    }

    /// TC-CLI-BROWSE-1: a whole turn on a screen with room for all of it.
    /// Expected: the body is the timeline's lines, in order, and nothing else.
    /// This is the promise the view rests on - a journal read full-screen has
    /// to be the same journal, or the two ways of reading one disagree about
    /// what happened.
    #[test]
    fn what_the_page_holds_is_the_timeline() {
        let events = turn();
        let want = timeline(&events, COLS);
        let mut view = journal(&events, COLS);

        assert_eq!(body(&rows(&mut view, COLS, want.len() + 6)), want);
    }

    /// TC-CLI-BROWSE-2: the same turn on a screen with room for four rows of it.
    /// Expected: the last four lines of the timeline, because the end of a
    /// journal is where a reader opening one starts. Then Home, and the first
    /// four - the reason the transcript is kept at all.
    #[test]
    fn a_journal_taller_than_the_screen_opens_at_its_end() {
        let events = turn();
        let want = timeline(&events, COLS);
        let mut view = journal(&events, COLS);

        assert_eq!(body(&rows(&mut view, COLS, 8)), want[want.len() - 4..]);

        view.key(Key::Home);
        assert_eq!(body(&rows(&mut view, COLS, 8)), want[..4]);

        view.key(Key::End);
        assert_eq!(body(&rows(&mut view, COLS, 8)), want[want.len() - 4..]);
    }

    /// TC-CLI-BROWSE-3: the terminal is made narrower while the view is open.
    /// Expected: the body is the timeline composed at the new width, not the
    /// old lines cut to fit it. A journal is finished, so its lines can be
    /// composed again; a live page's cannot, which is why `Page` itself does
    /// not do this.
    #[test]
    fn a_resize_composes_the_lines_again() {
        let events = vec![event(
            "user/message",
            json!({ "content": "a prompt long enough that where it wraps depends on how wide the screen is" }),
        )];
        let mut view = journal(&events, 70);

        let wide = timeline(&events, 70);
        let narrow = timeline(&events, 34);
        assert_ne!(wide, narrow, "the case needs a width the wrapping notices");

        assert_eq!(body(&rows(&mut view, 70, 20)), wide);
        assert_eq!(body(&rows(&mut view, 34, 20)), narrow);
    }

    /// TC-CLI-BROWSE-4: a screenful at a time, and one row at a time.
    /// Expected: PageUp moves the window by the height of the screen less the
    /// four rows of furniture and one line kept for the reader's place;
    /// PageDown comes back; Up and Down move one row. Asserted as the distance
    /// the window reports, because that is the state the key sets and the rows
    /// on screen are `Page`'s to arrange.
    #[test]
    fn the_keys_move_the_window_by_what_they_say() {
        let events = turn();
        let mut view = journal(&events, COLS);
        rows(&mut view, COLS, 12);

        view.key(Key::PageUp);
        assert_eq!(view.page.back(), 12 - KEPT);
        view.key(Key::Up);
        assert_eq!(view.page.back(), 12 - KEPT + 1);
        view.key(Key::Down);
        assert_eq!(view.page.back(), 12 - KEPT);
        view.key(Key::PageDown);
        assert_eq!(view.page.back(), 0);
    }

    /// TC-CLI-BROWSE-5: the keys that end the view, and one that does not.
    /// Expected: `q` and Esc stop, and a key with no meaning here is ignored
    /// rather than treated as either. Esc is included because a key this view
    /// does not name arrives as `Esc`, so it is the one that closes a view by
    /// accident if it is not deliberate.
    #[test]
    fn q_and_esc_close_the_view() {
        let events = turn();
        let mut view = journal(&events, COLS);

        assert_eq!(view.key(Key::Char('x')), Flow::Go);
        assert_eq!(view.key(Key::Char('q')), Flow::Stop);
        assert_eq!(view.key(Key::Esc), Flow::Stop);
    }
    /// The plain text of a frame's body, which is what a reader sees.
    fn text(rows: &[String]) -> String {
        body(rows).join("\n")
    }

    /// TC-CLI-BROWSE-6: `/`, a word two lines hold, and Enter; then `n`.
    /// Expected: the window moves to the first line holding the word and puts
    /// it at the top, so what follows the match is what the reader gets; the
    /// footer counts the matches; `n` reaches the second and wraps back round
    /// to the first. The window is asserted by what is on the page rather than
    /// by `back`, because where the reader lands is the promise.
    #[test]
    fn a_search_reaches_the_line_that_holds_it() {
        let events = turn();
        let mut view = journal(&events, COLS);
        rows(&mut view, COLS, 8);

        typed(&mut view, "echo");
        view.key(Key::Enter);
        let first = rows(&mut view, COLS, 8);
        assert!(body(&first)[0].contains("echo"), "{first:?}");
        assert!(first[first.len() - 1].contains("match 1 of"), "{first:?}");

        view.key(Key::Char('n'));
        let second = rows(&mut view, COLS, 8);
        assert!(text(&second).contains("echo"), "{second:?}");
        assert_ne!(body(&second), body(&first), "`n` stayed put");
        assert!(
            second[second.len() - 1].contains("match 2 of"),
            "{second:?}"
        );

        // Round the end and back to the first, which is the whole reason the
        // count is on the footer: without it a wrap looks like a view stuck.
        let hits = match &view.find {
            Find::On { hits, .. } => hits.len(),
            _ => 0,
        };
        for _ in 1..hits {
            view.key(Key::Char('n'));
        }
        assert_eq!(body(&rows(&mut view, COLS, 8))[0], body(&first)[0]);
    }

    /// TC-CLI-BROWSE-7: the keys while the prompt is open.
    /// Expected: `q` is a letter and does not close the view, Backspace takes
    /// one back, Esc puts the keys back without having moved the page, and
    /// nothing moves until Enter - a page that jumped on each keystroke would
    /// take the reader off the line they were reading to find it.
    #[test]
    fn the_prompt_takes_the_printable_keys_while_it_is_open() {
        let events = turn();
        let mut view = journal(&events, COLS);
        let before = rows(&mut view, COLS, 8);

        view.key(Key::Char('/'));
        assert_eq!(view.key(Key::Char('q')), Flow::Go, "`q` closed the view");
        view.key(Key::Backspace);
        for letter in "echo".chars() {
            view.key(Key::Char(letter));
        }
        assert!(view.typing(), "the prompt closed under a word");
        assert_eq!(body(&rows(&mut view, COLS, 8)), body(&before), "it moved");

        view.key(Key::Esc);
        assert!(!view.typing());
        assert_eq!(body(&rows(&mut view, COLS, 8)), body(&before));
        assert_eq!(
            view.key(Key::Char('q')),
            Flow::Stop,
            "`q` is a letter still"
        );
    }

    /// TC-CLI-BROWSE-8: a word no line holds, and `n` before any search.
    /// Expected: the footer says so, the page has not moved, and the view is
    /// still open - a search that found nothing is an answer, not a failure.
    /// `n` with no search behind it is ignored like any other unknown key.
    #[test]
    fn a_word_no_line_holds_is_said_rather_than_acted_on() {
        let events = turn();
        let mut view = journal(&events, COLS);
        let before = rows(&mut view, COLS, 8);

        assert_eq!(view.key(Key::Char('n')), Flow::Go);
        assert_eq!(body(&rows(&mut view, COLS, 8)), body(&before));

        typed(&mut view, "zebra");
        view.key(Key::Enter);
        let after = rows(&mut view, COLS, 8);
        assert!(
            after[after.len() - 1].contains("no line holds zebra"),
            "{after:?}"
        );
        assert_eq!(body(&after), body(&before), "the page moved on no match");
    }

    /// TC-CLI-BROWSE-9: the terminal is made narrower with a search on.
    /// Expected: the search is made again against the lines as they are now,
    /// so the match the footer counts is a line that holds the word at the new
    /// width. A rewrap moves every line, and hit numbers kept over one would
    /// point at whatever had slid into their place.
    #[test]
    fn a_resize_finds_the_lines_again() {
        let events = turn();
        let mut view = journal(&events, 70);
        rows(&mut view, 70, 8);

        typed(&mut view, "echo");
        view.key(Key::Enter);
        assert!(text(&rows(&mut view, 70, 8)).contains("echo"));

        let narrow = rows(&mut view, 34, 8);
        assert!(body(&narrow)[0].contains("echo"), "{narrow:?}");
        assert!(
            narrow[narrow.len() - 1].contains("match 1 of"),
            "{narrow:?}"
        );
    }

    /// TC-CLI-BROWSE-10: `?` over a journal, and the way back out of it.
    /// Expected: the card names the view, holds a row for every key the view
    /// answers including the one that opened it, and says how to leave; the
    /// transcript is not on it; and the next key - any key - gives back the
    /// frame that was there before, unmoved.
    #[test]
    fn the_card_says_every_key_and_any_key_takes_it_down() {
        let events = turn();
        let mut view = journal(&events, COLS);
        let before = rows(&mut view, COLS, 12);

        assert_eq!(view.key(Key::Char('?')), Flow::Go);
        let card = rows(&mut view, COLS, 12);
        let shown = text(&card);
        assert!(
            card[0].contains("journal keys"),
            "not headed as the card: {card:?}"
        );
        assert!(
            card[card.len() - 1].contains("any key goes back"),
            "no way back: {card:?}"
        );
        for said in [
            "one line back",
            "a screenful either way",
            "go to the line holding it",
            "the next line holding it",
            "close the view",
            "this card",
        ] {
            assert!(
                shown.contains(said),
                "the card does not say {said}: {card:?}"
            );
        }
        assert!(
            !shown.contains("echo this"),
            "the transcript is still on it: {card:?}"
        );

        // Not `?` and not `q`: a reader who opened it by accident is one key
        // from where they were, whichever key they try.
        assert_eq!(view.key(Key::Char('z')), Flow::Go);
        assert_eq!(
            rows(&mut view, COLS, 12),
            before,
            "the card left the page moved"
        );
    }

    /// TC-CLI-BROWSE-11: `?` while a word is being spelled.
    /// Expected: it is a letter of the word, the card does not open, and the
    /// prompt reads back what was typed.
    #[test]
    fn the_prompt_keeps_the_key_that_would_open_the_card() {
        let events = turn();
        let mut view = journal(&events, COLS);
        typed(&mut view, "ec?");

        let shown = rows(&mut view, COLS, 12);
        assert!(
            !shown[0].contains("journal keys"),
            "the card opened: {shown:?}"
        );
        assert!(
            shown[shown.len() - 1].contains("/ec?"),
            "the ? did not go into the word: {shown:?}"
        );
    }

    /// TC-CLI-BROWSE-12: the footer on a terminal too narrow for its keys.
    /// Expected: at 60 columns the whole key list is on it; at 30 the two keys
    /// that cannot be lost are, and nothing is cut mid-word.
    #[test]
    fn a_narrow_footer_keeps_the_card_and_the_way_out() {
        let events = turn();
        let mut view = journal(&events, COLS);
        let wide = rows(&mut view, COLS, 12);
        let footer = wide[wide.len() - 1].clone();
        assert!(
            footer.contains("scroll"),
            "the wide footer lost the keys: {footer}"
        );
        assert!(
            footer.contains("? keys"),
            "the wide footer never says `?`: {footer}"
        );

        let narrow = rows(&mut view, 30, 12);
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
            !footer.contains("scroll"),
            "the narrow footer kept the long wording: {footer}"
        );
    }

    /// TC-CLI-BROWSE-13: the word a search found, marked where it is drawn.
    /// Expected: every line on the page that draws the word has it marked, not
    /// only the line the window landed on, and in whatever case it was
    /// written; the marks come off when the search is dropped; and what the
    /// page draws underneath is the same text throughout - a mark is escapes,
    /// and a mark that changed a line would move every line under it.
    #[test]
    fn a_search_marks_the_word_where_it_is_drawn() {
        let events = turn();
        // Tall enough to hold the whole transcript, so the window cannot move
        // under the comparison: what changes here is the marks and nothing else.
        let tall = timeline(&events, COLS).len() + 6;
        let mut view = painted(&events, COLS);
        let before: Vec<String> = body(&rows(&mut view, COLS, tall))
            .iter()
            .map(|row| plain(row))
            .collect();

        typed(&mut view, "echo");
        view.key(Key::Enter);
        let found = body(&rows(&mut view, COLS, tall));
        let marked = found
            .iter()
            .filter(|row| row.contains("\u{1b}[7mecho\u{1b}[27m"))
            .count();
        let holding = found
            .iter()
            .filter(|row| plain(row).to_lowercase().contains("echo"))
            .count();
        assert!(marked > 1, "one line marked at most: {found:?}");
        assert_eq!(marked, holding, "a line holds the word unmarked: {found:?}");
        let after: Vec<String> = found.iter().map(|row| plain(row)).collect();
        assert_eq!(after, before, "the mark changed what the page draws");

        // Dropped: the prompt opened and abandoned takes the marks with it.
        view.key(Key::Char('/'));
        view.key(Key::Esc);
        let dropped = body(&rows(&mut view, COLS, tall));
        assert!(
            !dropped.iter().any(|row| row.contains("\u{1b}[7m")),
            "{dropped:?}"
        );
        assert_eq!(
            dropped.iter().map(|row| plain(row)).collect::<Vec<_>>(),
            before
        );
    }

    /// TC-CLI-BROWSE-14: a search on a terminal that was given no colour.
    /// Expected: the search still works and the footer still counts it, and
    /// the page is byte for byte what it was - not one escape. `--color never`
    /// is a promise about the bytes, and a mark is the one thing on the page
    /// that a renderer writes rather than a theme.
    #[test]
    fn a_plain_terminal_is_never_marked() {
        let events = turn();
        let tall = timeline(&events, COLS).len() + 6;
        let mut view = journal(&events, COLS);
        let before = body(&rows(&mut view, COLS, tall));

        typed(&mut view, "echo");
        view.key(Key::Enter);
        let found = rows(&mut view, COLS, tall);
        assert!(found[found.len() - 1].contains("match 1 of"), "{found:?}");
        assert_eq!(body(&found), before);
    }
}
