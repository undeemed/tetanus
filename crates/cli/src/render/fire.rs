//! The conversation on a screen of its own: a transcript, and a line to type
//! into at the foot of it.
//!
//! `tetanus chat` holds the terminal for the length of one line and gives it
//! back for the length of one turn, so the conversation is written into the
//! reader's scrollback the way a shell writes into it. That is the right shape
//! for a command a person runs between other commands, and the wrong one for a
//! conversation they stay in: the answer scrolls past, going back through it
//! means leaving the chat, and the row they type on is wherever the last turn
//! happened to end.
//!
//! This is the other shape. The whole terminal is the conversation: the
//! transcript above, scrollable and kept, the turn arriving in the block that
//! is pinned under it, and one row at the foot that is always the row you
//! type on, wherever the reader has scrolled to.
//!
//! # Composition
//!
//! ```text
//! tetanus                                   chat on mock-echo-1   heading
//!                                                                 blank
//! turn 1                                                       |
//!   you   run one full turn                                    |  transcript
//!   ai    the answer as the chunks assemble it                 |
//!   ⠹ streaming the answer · 1.2s                              |  block
//!                                                                 blank
//! > what happens if I ask this                                    prompt
//! enter ask · pgup/pgdn back · ctrl-c leave           2 back      footer
//! ```
//!
//! # What this module does not do
//!
//! It does not read the terminal, run a turn, or look at a clock. It is handed
//! keys, events and the time a turn has been running, and it hands back a
//! frame and what the reader asked for. That is what makes every case below a
//! function of its inputs rather than a session driven through a pty.
//!
//! # Why a resize composes the conversation again
//!
//! A line was folded for the width it was written at, and a terminal that
//! changes size makes that width a lie. Cut to a narrower frame it loses its
//! tail; left alone on a wider one it keeps folds the window no longer needs,
//! and the row that carried the tail of a fold reads as a sentence of its own.
//!
//! So this view keeps what was said rather than the rows it drew, and composes
//! the rows again whenever the width changes. `Page` does not, and says why: a
//! live view must not rewrite history under a reader who is still reading it.
//! A resize is not the stream rewriting anything - it is the reader asking for
//! a new shape, at the moment they ask - which is the same reading
//! [`browse`](super::browse) makes of the same rule.
//!
//! # Why a search is a command and not a key
//!
//! Every other full-screen view in this binary opens its search with `/`,
//! because `/` is a key there. Here it is a character in the line being typed,
//! and a view that took it is a view where a reader cannot ask about a path.
//! So the search is `/find word` - a command, in the vocabulary this chat
//! already has - and the keys that walk the matches are two the editor does
//! not answer: ctrl-n for the next, ctrl-p for the one before.
//!
//! # Why the editor answers first
//!
//! Every printable key belongs to the line being typed - including `?` and
//! `q`, which are keys on every other full-screen view this binary has. A view
//! that took them would be a view where a reader cannot type a question. So a
//! key goes to [`Line`] first, and only the ones it does not answer -
//! the arrows, the page keys, Escape - are the view's own.
//!
//! The commands are the ones `tetanus chat` already answers, `/help` and
//! `/exit` among them, because a reader who knows the chat should not have to
//! learn a second vocabulary to use the same chat on a screen.

use std::time::Duration;

use tetanus_protocol::rpc::RpcError;
use tetanus_protocol::types::SessionEvent;
use tetanus_ui::{
    bar, light, plain, tame_line, visible_width, Frame, Key, Line, Role, Theme, Typed,
};

use super::live::Live;

/// Rows the arrangement spends on furniture: the heading, the blank under it,
/// the blank over the prompt, the prompt itself, and the footer.
const CHROME: usize = 5;

/// The marker the prompt row opens with, and what it costs in columns.
const MARKER: &str = "> ";

/// What the reader asked for with the key they just pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Nothing that leaves this view: keep going, and paint.
    Go,
    /// A line was finished. The caller runs it as a turn, or reads it as one
    /// of the chat's own commands.
    Asked(String),
    /// Ctrl-D on an empty line, or `/exit`: the reader is leaving, and the
    /// conversation ended the way it was meant to.
    Leave,
    /// Ctrl-C: the reader is leaving now. §4.5 gives an interrupted command
    /// 130, and the ordinary chat gives the same key the same status, so a
    /// script wrapping either reads one answer.
    Stopped,
}

/// The conversation, as one screen.
/// One thing that was said, in the form it was said in.
///
/// The rows a frame draws are composed from these and thrown away; these are
/// kept. A width is a property of the terminal at the moment of drawing, and
/// anything stored already folded is stored wrong the moment the window
/// changes size.
enum Said {
    /// An event off the journal: the model's, the reader's, or a tool's.
    Event(Box<SessionEvent>),
    /// The card `/help` prints.
    Card,
    /// One line of this build's own words - a command it does not have, a
    /// flush that did not work.
    Note(String),
    /// A turn that failed, worded the way every other surface words it.
    Fault(Box<RpcError>),
}

pub struct Fire {
    theme: Theme,
    /// What the heading says on the right: the model this chat is on.
    title: String,
    /// The conversation, as what was said rather than as what was drawn.
    /// Nothing is dropped: the alternate screen has no scrollback, so a view
    /// that let a line go has lost it.
    said: Vec<Said>,
    /// The rows those make at the width the last frame was built for.
    lines: Vec<String>,
    /// How far back through the transcript the reader has scrolled, in rows.
    /// Zero is the foot of it, which is where an arriving line lands.
    back: usize,
    /// The line being typed, and where its cursor is.
    line: Line,
    /// The composer every line goes through: the settled ones, and the block
    /// that says what a running turn is waiting on. Always there, because a
    /// resize composes the whole conversation again and a composer that only
    /// existed while a turn ran would have nothing to compose it with.
    live: Live,
    /// Whether the line the reader typed is still being answered.
    working: bool,
    /// The last size a frame was built for, so a scroll by a screenful knows
    /// how big a screenful is.
    body: usize,
    width: usize,
    think: bool,
    /// What the block says the turn is waiting on, kept so that a composer
    /// built again after a resize opens on the phase the last one was in.
    phase: String,
    /// The word the reader is looking for, lower-cased, when they are.
    wanted: Option<String>,
    /// Which lines hold it, oldest first, and which of those the reader is on.
    /// Line numbers rather than marked strings, because a resize composes
    /// every line again and the numbers are what a rewrap can be redone from.
    hits: Vec<usize>,
    at: usize,
}

impl Fire {
    /// A view over a conversation that has not started yet.
    pub fn new(theme: Theme, width: usize, model: &str, think: bool) -> Self {
        // The composer opens finished: a conversation nobody has asked
        // anything in yet is not waiting on a turn, and the block that says
        // what a turn is waiting on has nothing to say until one starts.
        let mut live = Live::new(theme, width, "", think);
        live.finish();
        Self {
            theme,
            title: format!("chat on {}", tame_line(model)),
            said: Vec::new(),
            lines: Vec::new(),
            back: 0,
            line: Line::new(),
            live,
            working: false,
            body: 0,
            width,
            think,
            phase: String::new(),
            wanted: None,
            hits: Vec::new(),
            at: 0,
        }
    }

    /// Say one thing: put it on the conversation, and on the page.
    ///
    /// The window does not move for it. A reader who has scrolled back keeps
    /// what is under their eye while the answer settles underneath, which is
    /// the same promise [`Page`](tetanus_ui::Page) makes and for the same
    /// reason: the alternative drags the page out from under them, one row per
    /// arriving line.
    fn say(&mut self, said: Said) {
        let lines = self.compose(&said, self.width);
        if self.back > 0 {
            self.back += lines.len();
        }
        let from = self.lines.len();
        self.lines.extend(lines);
        self.said.push(said);
        self.mark(from);
    }

    /// Look for `word` in what has been said, or - given nothing - stop
    /// looking.
    ///
    /// The window goes to the newest match, because a conversation is read
    /// from its foot and the match a reader means is almost always the last
    /// one. The others are behind ctrl-p.
    pub fn find(&mut self, word: &str) {
        self.wanted = match word.trim().is_empty() {
            true => None,
            false => Some(word.trim().to_lowercase()),
        };
        // Composed again rather than marked in place: a mark is escapes
        // written into a line, and a line that already carries one cannot be
        // marked for a different word without being built again.
        self.fill(self.width);
        self.at = self.hits.len().saturating_sub(1);
        self.land();
    }

    /// Walk to the next match, or the one before it.
    fn walk(&mut self, forward: bool) {
        if self.hits.is_empty() {
            return;
        }
        // Round rather than stopping: a reader who reaches the last match and
        // asks for another is asking to go round, not to be told they cannot.
        self.at = match forward {
            true => (self.at + 1) % self.hits.len(),
            false => (self.at + self.hits.len() - 1) % self.hits.len(),
        };
        self.land();
    }

    /// Put the window where the match the reader is on can be read.
    fn land(&mut self) {
        let Some(line) = self.hits.get(self.at).copied() else {
            return;
        };
        // The match at the foot of the body, not the top of it: what was said
        // before a match is the context a reader wants with it, and what comes
        // after is what they are about to scroll to anyway.
        let room = self.body.max(1);
        self.back = self.lines.len().saturating_sub(line + 1);
        self.back = self
            .back
            .min(self.lines.len().saturating_sub(room.min(self.lines.len())));
    }

    /// Mark the lines from `from` on, and remember which of them hold the
    /// word.
    ///
    /// With colour off there is nothing to mark with - `--color never` is a
    /// promise about the bytes - so the lines are found and not painted, and
    /// the walk still works.
    fn mark(&mut self, from: usize) {
        let Some(word) = self.wanted.clone() else {
            return;
        };
        let painted = self.theme.color();
        for at in from..self.lines.len() {
            if !plain(&self.lines[at]).to_lowercase().contains(&word) {
                continue;
            }
            self.hits.push(at);
            if painted {
                self.lines[at] = light(&self.lines[at], &word);
            }
        }
    }

    /// The card `/help` prints, on the conversation.
    pub fn card(&mut self) {
        self.say(Said::Card);
    }

    /// One line of this build's own words.
    pub fn note(&mut self, said: &str) {
        self.say(Said::Note(said.to_string()));
    }

    /// A turn that failed, worded the way every other surface words it.
    pub fn fault(&mut self, error: &RpcError) {
        self.say(Said::Fault(Box::new(error.clone())));
    }

    /// The rows one thing said makes at `cols`.
    ///
    /// Every kind of thing on this page composes here, so a resize has one
    /// place to go through and no kind of line can be the one that was left
    /// folded for the old width.
    fn compose(&mut self, said: &Said, cols: usize) -> Vec<String> {
        match said {
            Said::Event(event) => self.live.push(event),
            Said::Card => super::chat::card(&self.theme, cols),
            Said::Note(text) => {
                vec![self.theme.paint(Role::Warn, text).to_string()]
            }
            Said::Fault(error) => super::fault::lines(&self.theme, cols, error),
        }
    }

    /// Compose the whole conversation again, for a terminal that changed size.
    ///
    /// The composer is built again with it, because a block folded for the old
    /// width is as wrong as a settled line folded for it, and the phase it was
    /// in is what it opens on.
    ///
    /// How far back the reader had scrolled is kept, in rows. A rewrap moves
    /// every line, so a row count means something slightly different
    /// afterwards; that is the price of the alternative, which is a page that
    /// is visibly wrong until the reader scrolls it.
    fn fill(&mut self, cols: usize) {
        let back = self.back;
        let said = std::mem::take(&mut self.said);
        self.lines.clear();
        self.hits.clear();
        self.back = 0;
        self.live = Live::new(self.theme, cols, &self.phase, self.think);
        for one in &said {
            let lines = self.compose(one, cols);
            self.lines.extend(lines);
        }
        // A view with no turn running has no block, and says so the way the
        // turn ending said it.
        if !self.working {
            self.live.finish();
        }
        self.said = said;
        self.mark(0);
        self.back = back;
        self.width = cols;
    }

    /// The model this conversation is on, as the heading says it.
    pub fn model(&self) -> &str {
        self.title.trim_start_matches("chat on ")
    }

    /// Put a conversation that was already on the journal onto the page.
    ///
    /// A resumed chat opens on what was said before it, because the screen is
    /// the conversation and a conversation that started this morning did not
    /// start empty. The events go through a composer of their own and settle
    /// as ordinary lines: nothing about them is live, and the block that says
    /// what a turn is waiting on has nothing to say about a turn that ended.
    pub fn history(&mut self, events: &[SessionEvent]) {
        for event in events {
            self.say(Said::Event(Box::new(event.clone())));
        }
    }

    /// A turn has begun: what arrives from here is drawn as it arrives.
    pub fn started(&mut self, phase: &str) {
        self.phase = phase.to_string();
        self.live = Live::new(self.theme, self.width, phase, self.think);
        self.working = true;
        // The reader asked for this turn, so they are shown it: a question
        // sent from three screens back would otherwise be answered off-screen.
        self.follow();
    }

    /// One event off the journal, while a turn is running.
    pub fn push(&mut self, event: &SessionEvent) {
        self.say(Said::Event(Box::new(event.clone())));
    }

    /// The turn is over, however it ended.
    pub fn finished(&mut self) {
        self.live.finish();
        self.working = false;
    }

    /// Advance the spinner. The caller's clock, not this module's.
    pub fn tick(&mut self) {
        self.live.tick();
    }

    /// Read one keystroke.
    ///
    /// The editor answers first, so every printable key is a character in the
    /// line. What it does not answer is this view's: the arrows and the page
    /// keys move the window, and Escape brings it back to the foot.
    pub fn key(&mut self, key: Key) -> Act {
        // While a turn is running the reader may keep typing - the next
        // question is often the answer to what they are watching - but Enter
        // is not offered to the editor, which would hand the line over and
        // leave the row empty. A second prompt against a busy session is
        // refused by the engine, and a refusal landing in the transcript
        // reads as the question having been swallowed.
        if self.working && key == Key::Enter {
            return Act::Go;
        }
        match self.line.key(key) {
            Typed::Editing => Act::Go,
            Typed::Interrupted => Act::Stopped,
            Typed::Left => Act::Leave,
            // An empty line is a keypress, not a question. The chat below
            // says the same about one typed at a shell prompt.
            Typed::Asked(said) => match said.trim().is_empty() {
                true => Act::Go,
                false => Act::Asked(said),
            },
            Typed::Ignored => self.moved(key),
            _ => Act::Go,
        }
    }

    /// The keys the editor does not answer: the way back through what was
    /// said, and the way to the end of it.
    fn moved(&mut self, key: Key) -> Act {
        let screenful = self.body.saturating_sub(1).max(1);
        match key {
            Key::Up => self.scroll(1),
            Key::Down => self.scroll(-1),
            Key::PageUp => self.scroll(screenful as isize),
            Key::PageDown => self.scroll(-(screenful as isize)),
            Key::Ctrl('n') => self.walk(true),
            Key::Ctrl('p') => self.walk(false),
            // Back to the foot, and out of a search: both are the reader
            // saying they are done looking at what they were looking at.
            Key::Esc => {
                self.find("");
                self.follow();
            }
            _ => {}
        }
        Act::Go
    }

    /// Move the window back through the transcript, or forward towards its
    /// foot. Positive is back, which is the direction a reader looking for
    /// something already said is going.
    fn scroll(&mut self, rows: isize) {
        let back = self.back as isize + rows;
        self.back = back.max(0) as usize;
    }

    /// Back to the foot of the transcript, where arriving lines land.
    fn follow(&mut self) {
        self.back = 0;
    }

    /// The whole screen as of now, for a turn that has been running `spent`.
    pub fn frame(&mut self, cols: usize, rows: usize, spent: Duration) -> Frame {
        if cols != self.width {
            self.fill(cols);
        }
        let block = self.live.block(spent);
        let body = rows.saturating_sub(CHROME);
        self.body = body;
        let block = &block[block.len().saturating_sub(body)..];

        let mut frame = Frame::new(cols, rows);
        frame.row(bar(
            cols,
            &self.theme.paint(Role::Heading, "tetanus").to_string(),
            &self.theme.paint(Role::Muted, &self.title).to_string(),
        ));
        frame.blank();
        for line in self.window(body - block.len()) {
            frame.row(line);
        }
        for line in block {
            frame.row(line);
        }
        // What is left goes above the prompt, so a conversation that has just
        // started sits at the top of the screen rather than the middle.
        while frame.free() > 2 {
            frame.blank();
        }

        let label = visible_width(MARKER);
        let (typed, cursor) = self.line.shown(cols.saturating_sub(label));
        frame.row(format!("{}{typed}", self.theme.paint(Role::Accent, MARKER)));
        frame.row(self.footer(cols));
        // The one row on this screen the terminal's own cursor belongs on.
        frame.cursor(rows.saturating_sub(2), label + cursor);
        frame
    }

    /// The slice of the transcript this frame shows, `room` rows of it.
    fn window(&mut self, room: usize) -> &[String] {
        self.back = self.back.min(self.lines.len().saturating_sub(room));
        let end = self.lines.len() - self.back;
        &self.lines[end.saturating_sub(room)..end]
    }

    /// The keys on the left, where the reader is on the right.
    fn footer(&self, cols: usize) -> String {
        let dot = self.theme.glyph("·", "-");
        let full = match self.wanted.is_some() {
            // The keys that matter while a search is up are the ones that walk
            // it, and they are not on the footer the rest of the time.
            true => format!("ctrl-n next {dot} ctrl-p back {dot} esc done {dot} ctrl-c leave"),
            false => format!(
                "enter ask {dot} {} scroll {dot} /help {dot} /find {dot} ctrl-c leave",
                self.theme.glyph("↑↓", "up/dn")
            ),
        };
        let keys = super::keys::hint(cols, &full, &format!("enter ask {dot} ctrl-c leave"));
        let here = match (&self.wanted, self.back, self.working) {
            // A search is what the reader is doing, so it has the corner the
            // position would have had. `0 of 0` is not a place: a word no line
            // holds is said in words.
            (Some(word), _, _) => match self.hits.is_empty() {
                true => format!("no line holds {}", tame_line(word)),
                false => format!("{} of {}", self.at + 1, self.hits.len()),
            },
            (None, 0, true) => "working".to_string(),
            (None, 0, false) => "end".to_string(),
            (None, back, _) => format!("{back} back"),
        };
        let role = match (self.working, self.wanted.is_some() && self.hits.is_empty()) {
            (_, true) => Role::Warn,
            (true, _) => Role::Accent,
            (false, _) => Role::Muted,
        };
        bar(
            cols,
            &self.theme.paint(Role::Muted, &keys).to_string(),
            &self.theme.paint(role, &here).to_string(),
        )
    }
}

/// Test Design Specification: the conversation as one screen.
///
/// Features tested: the arrangement of a frame and where the cursor lands on
/// it; that `/find` counts its matches, lands on the newest and says when
/// there are none, and that ctrl-n and ctrl-p walk them and go round; that every printable key belongs to the line being typed and the keys
/// the editor does not answer move the window; that Enter asks and an empty
/// line does not; that a line typed while a turn is running is held rather
/// than sent; that Ctrl-C and Ctrl-D are told apart; that arriving lines do
/// not move a window a reader has scrolled back; that a resize composes the
/// conversation again and a search open at the time survives it; and that a
/// line longer than the row keeps the cursor on the row.
///
/// Features NOT tested here: what a turn's lines say (owned by
/// `render::timeline` and `render::live`), what the editor does with a key
/// (owned by `tetanus_ui::Line`), how a frame is painted (owned by
/// `tetanus_ui::Frame`), and the loop that reads the terminal and runs the
/// turns (owned by `chat`, and driven end to end by `target/probe-fire.py`).
///
/// Environmental needs: none. Every case builds frames in memory: no terminal,
/// no journal, no clock.
#[cfg(test)]
mod tests {
    use tetanus_ui::{buffered, Charset};

    use super::*;

    const COLS: usize = 60;
    const ROWS: usize = 10;

    fn theme() -> Theme {
        Theme::new(false, Charset::Unicode)
    }

    fn fire() -> Fire {
        Fire::new(theme(), COLS, "mock-echo-1", false)
    }

    /// One frame, as rows of text, with the terminal's own control codes gone.
    fn rows(view: &mut Fire, rows: usize) -> Vec<String> {
        let frame = view.frame(COLS, rows, Duration::ZERO);
        let mut ui = buffered(theme(), COLS);
        frame.paint(&mut ui).expect("paint");
        ui.contents()
            .trim_start_matches("\x1b[H")
            .split("\r\n")
            .map(|row| {
                row.split('\x1b')
                    .next()
                    .unwrap_or_default()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// Where the paint puts the terminal's cursor, as (row, column), counted
    /// from zero the way a frame counts them.
    fn cursor(view: &mut Fire, rows: usize) -> (usize, usize) {
        let frame = view.frame(COLS, rows, Duration::ZERO);
        let mut ui = buffered(theme(), COLS);
        frame.paint(&mut ui).expect("paint");
        let painted = ui.contents();
        let at = painted.rfind('H').expect("a cursor move");
        let place = painted[..at]
            .rsplit_once("\x1b[")
            .expect("a cursor sequence")
            .1;
        let (row, col) = place.split_once(';').expect("row;col");
        (
            row.parse::<usize>().expect("a row") - 1,
            col.parse::<usize>().expect("a column") - 1,
        )
    }

    /// One frame at a stated width, as rows of text.
    fn rows_at(view: &mut Fire, cols: usize, rows: usize) -> Vec<String> {
        let frame = view.frame(cols, rows, Duration::ZERO);
        let mut ui = buffered(theme(), cols);
        frame.paint(&mut ui).expect("paint");
        ui.contents()
            .trim_start_matches("\x1b[H")
            .split("\r\n")
            .map(|row| {
                row.split('\x1b')
                    .next()
                    .unwrap_or_default()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// A journal event carrying one message.
    fn said_by(who: &str, text: &str) -> SessionEvent {
        SessionEvent {
            ty: format!("{who}/message").replace("you/", "user/"),
            seq: 1,
            time: 0,
            data: serde_json::json!({ "content": text, "turn": 1 }),
            source_event_seqs: None,
        }
    }

    fn typing(view: &mut Fire, text: &str) {
        for char in text.chars() {
            assert_eq!(view.key(Key::Char(char)), Act::Go);
        }
    }

    /// TC-CLI-FIRE-1: a conversation with two settled lines, on a ten-row
    /// terminal.
    /// Expected: the heading on the first row, the transcript under it, the
    /// prompt on the second-to-last row with its marker, the footer last, and
    /// the terminal's cursor on the prompt row, past the marker and the text.
    /// The prompt row is the one row of this screen whose position a reader
    /// relies on: it is where the next character lands, whatever is above it.
    #[test]
    fn the_prompt_is_the_second_to_last_row_and_the_cursor_is_on_it() {
        let mut view = fire();
        view.note("turn 1");
        view.note("  you   hello");
        typing(&mut view, "next");

        let rows = rows(&mut view, ROWS);
        assert_eq!(rows.len(), ROWS, "not the height asked for: {rows:?}");
        assert!(rows[0].starts_with("tetanus"), "no heading: {rows:?}");
        assert!(rows[0].ends_with("chat on mock-echo-1"), "{rows:?}");
        assert_eq!(rows[ROWS - 2], "> next", "not the prompt: {rows:?}");
        assert!(rows[ROWS - 1].contains("enter ask"), "no footer: {rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("you   hello")),
            "the transcript is missing: {rows:?}"
        );
        assert_eq!(cursor(&mut view, ROWS), (ROWS - 2, "> next".len()));
    }

    /// TC-CLI-FIRE-2: Enter on a line with something on it, and on one
    /// without.
    /// Expected: the line comes back once and the editor is empty after it; an
    /// empty line is a keypress and not a question, which is what a reader
    /// pressing Enter to see the screen redraw expects.
    #[test]
    fn enter_asks_what_was_typed_and_nothing_when_nothing_was() {
        let mut view = fire();
        typing(&mut view, "what is this");

        assert_eq!(view.key(Key::Enter), Act::Asked("what is this".into()));
        assert_eq!(view.key(Key::Enter), Act::Go);
        assert_eq!(rows(&mut view, ROWS)[ROWS - 2], ">");
    }

    /// TC-CLI-FIRE-3: a line finished while a turn is still being answered.
    /// Expected: it is not sent. A second prompt against a busy session is
    /// refused by the engine, and a refusal arriving in the transcript reads
    /// as the question having been swallowed - so the line stays where it is,
    /// and the footer says the view is working.
    #[test]
    fn a_line_typed_while_a_turn_runs_is_held_rather_than_sent() {
        let mut view = fire();
        view.started("running the turn on mock-echo-1");
        typing(&mut view, "and another thing");

        assert_eq!(view.key(Key::Enter), Act::Go);
        let rows = rows(&mut view, ROWS);
        assert_eq!(rows[ROWS - 2], "> and another thing", "{rows:?}");
        assert!(rows[ROWS - 1].contains("working"), "{rows:?}");

        view.finished();
        assert_eq!(
            view.key(Key::Enter),
            Act::Asked("and another thing".into()),
            "the line was lost when the turn ended"
        );
    }

    /// TC-CLI-FIRE-4: the keys the editor does not answer, and a line arriving
    /// while the window is back.
    /// Expected: the window moves and stays where it was put; Escape brings it
    /// back to the foot; and a line settling while a reader is reading does
    /// not drag the page out from under them.
    #[test]
    fn the_window_moves_only_when_the_reader_moves_it() {
        let mut view = fire();
        for n in 1..=40 {
            view.note(&format!("line {n}"));
        }
        rows(&mut view, ROWS);

        assert_eq!(view.key(Key::Up), Act::Go);
        assert_eq!(view.key(Key::Up), Act::Go);
        let back = rows(&mut view, ROWS);
        assert!(back[ROWS - 1].contains("2 back"), "{back:?}");

        view.note("line 41");
        let after = rows(&mut view, ROWS);
        assert_eq!(
            back[2], after[2],
            "an arriving line moved the window: {after:?}"
        );

        assert_eq!(view.key(Key::Esc), Act::Go);
        let end = rows(&mut view, ROWS);
        assert!(end[ROWS - 1].contains("end"), "{end:?}");
        assert!(
            end.iter().any(|row| row.contains("line 41")),
            "the foot is not the newest line: {end:?}"
        );
    }

    /// TC-CLI-FIRE-7: the same conversation at two widths.
    /// Expected: the rows are composed again for the width the frame is built
    /// at - nothing is cut with an ellipsis, and no fold from the old width
    /// survives into the new one. A line folded for a hundred columns and then
    /// drawn on forty-six loses its tail to the cut, and the row that carried
    /// the tail of the old fold reads as a sentence of its own.
    #[test]
    fn a_resize_composes_the_conversation_again() {
        let mut view = Fire::new(theme(), 100, "mock-echo-1", false);
        view.push(&said_by(
            "you",
            &"a question long enough to fold ".repeat(3),
        ));

        let wide = rows_at(&mut view, 100, ROWS);
        assert!(
            wide.iter()
                .any(|row| row.contains("a question long enough")),
            "{wide:?}"
        );

        let narrow = rows_at(&mut view, 46, ROWS);
        for row in &narrow {
            assert!(visible_width(row) <= 46, "`{row}` overruns 46");
        }
        assert!(
            !narrow.iter().any(|row| row.contains('\u{2026}')),
            "a row was cut instead of composed again: {narrow:?}"
        );
        // Every word survives the rewrap, which a cut does not allow.
        let said: String = narrow.join(" ");
        assert!(
            said.contains("a question long enough to fold"),
            "{narrow:?}"
        );

        // And back again: the wide frame is the wide frame, not the narrow
        // one with the folds still in it.
        let again = rows_at(&mut view, 100, ROWS);
        assert_eq!(wide, again, "widening did not undo the fold");
    }

    /// TC-CLI-FIRE-8: a card and a fault, narrowed and then widened again.
    /// Expected: every row fits the window it is drawn in, and the sentence
    /// the narrow window had to cut is whole again on the wide one. This build
    /// words those two rows itself, and a page where the model's lines are
    /// composed for the terminal and this build's are still composed for the
    /// terminal before it is a page with two rules.
    #[test]
    fn everything_on_the_page_is_composed_again() {
        let whole = "a sentence about a file long enough that a narrow window has to cut it";
        let mut view = Fire::new(theme(), 90, "mock-echo-1", false);
        view.card();
        view.fault(&RpcError::new(tetanus_protocol::rpc::ErrorCode::Io, whole));

        let wide = rows_at(&mut view, 90, ROWS + 6);
        assert!(
            wide.iter().any(|row| row.contains(whole)),
            "the fault is not whole at 90: {wide:?}"
        );

        let narrow = rows_at(&mut view, 44, ROWS + 6);
        for row in &narrow {
            assert!(visible_width(row) <= 44, "`{row}` overruns 44");
        }
        assert!(
            narrow.iter().any(|row| row.contains("/exit")),
            "the card is not on the narrow page: {narrow:?}"
        );

        let again = rows_at(&mut view, 90, ROWS + 6);
        assert_eq!(wide, again, "widening did not undo the cut");
    }

    /// TC-CLI-FIRE-9: `/find` on a conversation that holds the word twice, and
    /// on one that does not hold it at all.
    /// Expected: the footer counts the matches and says which one the reader
    /// is on, the window lands on the newest of them, and a word no line holds
    /// is said in words rather than counted as `0 of 0`.
    #[test]
    fn find_counts_the_matches_and_lands_on_the_newest() {
        let mut view = fire();
        for n in 1..=30 {
            view.note(&format!("line {n}"));
        }
        view.note("the word is alpha");
        for n in 31..=50 {
            view.note(&format!("line {n}"));
        }
        view.note("alpha again, at the foot");
        rows(&mut view, ROWS);

        view.find("alpha");
        let found = rows(&mut view, ROWS);
        assert!(found[ROWS - 1].contains("2 of 2"), "{found:?}");
        assert!(
            found.iter().any(|row| row.contains("alpha again")),
            "the window did not land on the newest match: {found:?}"
        );

        view.find("zzz");
        let none = rows(&mut view, ROWS);
        assert!(none[ROWS - 1].contains("no line holds zzz"), "{none:?}");
    }

    /// TC-CLI-FIRE-10: the keys that walk the matches.
    /// Expected: ctrl-n and ctrl-p move one match at a time and round at both
    /// ends - a reader who reaches the last match and asks for another is
    /// asking to go round, not to be told they cannot - and Escape ends the
    /// search and puts the window back at the foot.
    #[test]
    fn the_matches_are_walked_and_the_walk_goes_round() {
        let mut view = fire();
        for n in 1..=3 {
            view.note(&format!("alpha {n}"));
        }
        view.find("alpha");
        assert!(rows(&mut view, ROWS)[ROWS - 1].contains("3 of 3"));

        assert_eq!(view.key(Key::Ctrl('n')), Act::Go);
        assert!(
            rows(&mut view, ROWS)[ROWS - 1].contains("1 of 3"),
            "no round"
        );
        assert_eq!(view.key(Key::Ctrl('p')), Act::Go);
        assert!(
            rows(&mut view, ROWS)[ROWS - 1].contains("3 of 3"),
            "no round back"
        );

        assert_eq!(view.key(Key::Esc), Act::Go);
        assert!(
            rows(&mut view, ROWS)[ROWS - 1].contains("end"),
            "still searching"
        );
    }

    /// TC-CLI-FIRE-11: a search open when the terminal changes size.
    /// Expected: it survives. A rewrap moves every line, so the matches are
    /// found again on the lines that hold them now rather than kept as the
    /// numbers of lines that have moved - and a reader who widened their
    /// terminal did not ask to lose their search.
    #[test]
    fn a_search_survives_a_resize() {
        let mut view = Fire::new(theme(), 90, "mock-echo-1", false);
        view.push(&said_by("you", &"alpha ".repeat(24)));
        view.find("alpha");

        let wide = rows_at(&mut view, 90, ROWS);
        assert!(wide[ROWS - 1].contains(" of "), "{wide:?}");

        let narrow = rows_at(&mut view, 40, ROWS);
        assert!(
            narrow[ROWS - 1].contains(" of "),
            "the search was lost in the rewrap: {narrow:?}"
        );
        for row in &narrow {
            assert!(visible_width(row) <= 40, "`{row}` overruns 40");
        }
    }

    /// TC-CLI-FIRE-5: the two ways out, told apart.
    /// Expected: Ctrl-D on an empty line leaves, Ctrl-C stops. §4.5 gives an
    /// interrupted command 130 and an ordinary one 0, and the ordinary chat
    /// gives the same two keys the same two statuses.
    #[test]
    fn the_two_ways_out_are_not_the_same_way_out() {
        let mut view = fire();
        assert_eq!(view.key(Key::Ctrl('d')), Act::Leave);
        assert_eq!(view.key(Key::Ctrl('c')), Act::Stopped);

        // On a line with something on it, Ctrl-D is a delete and not a way
        // out - which is what every other line editor does with it.
        let mut typed = fire();
        typing(&mut typed, "hi");
        assert_eq!(typed.key(Key::Home), Act::Go);
        assert_eq!(typed.key(Key::Ctrl('d')), Act::Go);
        assert_eq!(rows(&mut typed, ROWS)[ROWS - 2], "> i");
    }

    /// TC-CLI-FIRE-6: a line longer than the row it is typed on.
    /// Expected: the row shows the window that holds the cursor, nothing
    /// overruns, and the cursor stays on the prompt row inside the terminal. A
    /// cursor placed past the edge is one the terminal puts where it likes.
    #[test]
    fn a_long_line_keeps_its_cursor_on_the_row() {
        let mut view = fire();
        typing(&mut view, &"typing on and on ".repeat(6));

        let rows = rows(&mut view, ROWS);
        for row in &rows {
            assert!(
                visible_width(row) <= COLS,
                "`{row}` overruns {COLS} columns"
            );
        }
        let (row, col) = cursor(&mut view, ROWS);
        assert_eq!(row, ROWS - 2, "the cursor left the prompt row");
        assert!(col < COLS, "the cursor is off the row: {col}");
    }
}
