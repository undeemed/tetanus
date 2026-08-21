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
//! tetanus                        chat on mock-echo-1 · 2 turns   heading
//!                                                                blank
//! turn 1                                                      |
//!   you   run one full turn                                   |  transcript
//!   ai    the answer as the chunks assemble it                |
//!   ⠹ streaming the answer · 1.2s                             |  block
//! ────────────────────────────────────────────────────────────   rule
//! > what happens if I ask this                                   prompt
//! enter ask · ↑↓ scroll · tab turn · /help · ctrl-c leave  2 back footer
//! ```
//!
//! The rule is the one row on the screen that says nothing. It is there
//! because the row under it is not part of the conversation: what a reader
//! types has not been said yet, and without a line between them a
//! half-written question reads as the newest thing on the transcript. The web
//! panel draws the same line for the same reason, and gets it from a border
//! rather than from a row.
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
//! # Why the up and down keys walk the history
//!
//! Because that is what they do at every other prompt a person has used, and
//! a chat is a prompt. The transcript keeps the page keys, which is what
//! scrolls a page everywhere else.
//!
//! What is half-typed when the walk starts is kept and comes back at the end
//! of it: a reader who pressed up to check what they asked last time has not
//! thrown away the question they were writing.
//!
//! # Why Tab walks the turns
//!
//! The web panel upstream puts every message in a list you can click. A
//! terminal has no list to put beside the conversation and no pointer to click
//! it with, and the thing a reader is actually reaching for is the same: the
//! start of a turn, three or thirty turns back. Tab and Shift-Tab are that
//! reach. They are the two keys left that mean "onwards" and "back" to
//! everybody, and the editor answers neither.
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
    bar, light, plain, tame_line, truncate, visible_width, wrap, Frame, Key, Line, Role, Theme,
    Typed,
};

use super::live::Live;

/// Rows the arrangement spends on furniture: the heading, the blank under it,
/// the rule over the prompt, one row of prompt, and the footer. A prompt that
/// has grown takes its extra rows from the transcript.
const CHROME: usize = 5;

/// The most rows a prompt may grow to.
///
/// Enough for a question somebody typed rather than pasted, and few enough
/// that the conversation is still the page. A longer line scrolls inside
/// these, the way the one row scrolled sideways.
const PROMPT: usize = 5;

/// The marker the prompt row opens with, and what it costs in columns.
const MARKER: &str = "> ";

/// Whether this is the event a turn opens on.
///
/// Read off the type rather than off the composed line, because what the line
/// says is a rendering and the type is the journal's own word (contract
/// §4.3.1).
fn opens_a_turn(said: &Said) -> bool {
    match said {
        Said::Event(event) => event.ty == "turn/start",
        _ => false,
    }
}

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
    /// The card `/keys` prints.
    Keys,
    /// The page a conversation with nothing asked in it yet opens on.
    Opening,
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
    /// The journal every turn is appended to, for the opening page. It is the
    /// one fact a reader needs to replay this conversation or resume it, and
    /// the ordinary chat prints it before the first question for that reason.
    journal: String,
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
    /// Whether the model's thinking is printed in full, and whether a tool's
    /// result is. Both are the reader's to change while they read: `/think`
    /// and `/more` toggle them, and the conversation is composed again.
    think: bool,
    whole: bool,
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
    /// The line each turn opens on, oldest first. Kept the same way and for
    /// the same reason as the matches: a rewrap moves them, and they are what
    /// Tab walks.
    turns: Vec<usize>,
    /// Every question this reader has asked, oldest first.
    history: Vec<String>,
    /// How far back through it they are, and what they were writing when they
    /// started walking. `None` is not walking, and the line is their own.
    walking: Option<(usize, String)>,
}

impl Fire {
    /// A view over a conversation that has not started yet.
    pub fn new(theme: Theme, width: usize, model: &str, journal: &str, think: bool) -> Self {
        // The composer opens finished: a conversation nobody has asked
        // anything in yet is not waiting on a turn, and the block that says
        // what a turn is waiting on has nothing to say until one starts.
        let mut live = Live::new(theme, width, "", think);
        live.finish();
        let mut view = Self {
            theme,
            title: format!("chat on {}", tame_line(model)),
            journal: tame_line(journal),
            said: Vec::new(),
            lines: Vec::new(),
            back: 0,
            line: Line::new(),
            live,
            working: false,
            body: 0,
            width,
            think,
            whole: false,
            phase: String::new(),
            wanted: None,
            hits: Vec::new(),
            at: 0,
            turns: Vec::new(),
            history: Vec::new(),
            walking: None,
        };
        view.say(Said::Opening);
        view
    }

    /// Say one thing: put it on the conversation, and on the page.
    ///
    /// The window does not move for it. A reader who has scrolled back keeps
    /// what is under their eye while the answer settles underneath, which is
    /// the same promise [`Page`](tetanus_ui::Page) makes and for the same
    /// reason: the alternative drags the page out from under them, one row per
    /// arriving line.
    fn say(&mut self, said: Said) {
        // A turn answers the opening page, and what it says - nothing asked
        // yet - stops being true the moment one exists. It goes rather than
        // scrolling away: a page that lies further up a transcript is worse
        // than one that was never there. However the turn arrived, including
        // off a journal this conversation is resuming.
        if opens_a_turn(&said) && self.said.iter().any(|said| matches!(said, Said::Opening)) {
            self.said.retain(|said| !matches!(said, Said::Opening));
            self.fill(self.width);
        }
        let lines = self.compose(&said, self.width);
        if self.back > 0 {
            self.back += lines.len();
        }
        let from = self.lines.len();
        if opens_a_turn(&said) && !lines.is_empty() {
            self.turns.push(from);
        }
        self.lines.extend(lines);
        self.said.push(said);
        self.mark(from);
    }

    /// Put the window on the turn before the one it is on, or the one after.
    ///
    /// The turn's own first line goes to the top of the body, because what
    /// follows a turn's opening is the turn, and a reader who asked for it
    /// wants to read forwards from there.
    fn turn(&mut self, forward: bool) {
        if self.turns.is_empty() {
            return;
        }
        let room = self.body.max(1);
        // Where the top of the body is now, as a line number.
        let top = self.lines.len().saturating_sub(self.back + room);
        let next = match forward {
            true => self.turns.iter().find(|line| **line > top).copied(),
            false => self.turns.iter().rev().find(|line| **line < top).copied(),
        };
        let Some(line) = next else {
            // Past the last turn is the foot of the conversation, which is
            // where a reader walking forwards is heading; before the first is
            // the top of it. Neither is a refusal to move.
            match forward {
                true => self.follow(),
                false => self.back = self.lines.len().saturating_sub(room.min(self.lines.len())),
            }
            return;
        };
        self.back = self
            .lines
            .len()
            .saturating_sub(line + room)
            .min(self.lines.len().saturating_sub(room.min(self.lines.len())));
    }

    /// Unfold the model's thinking, or fold it back. Answers what it is now.
    ///
    /// The conversation is composed again for it, which is what makes this a
    /// reader's decision rather than a flag they had to know about before
    /// they started: `--think` is the flag, and this is the same view changing
    /// its mind about what it has already drawn.
    pub fn thinking(&mut self) -> bool {
        self.think = !self.think;
        self.fill(self.width);
        self.think
    }

    /// Print tool results whole, or cap them again. Answers what it is now.
    ///
    /// A capped result says how many lines it is hiding, and a reader who
    /// wants those came for the output rather than for the answer it led to.
    /// The browser panel opens the same card; this is the terminal's way of
    /// opening it.
    pub fn whole(&mut self) -> bool {
        self.whole = !self.whole;
        self.fill(self.width);
        self.whole
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

    /// The card `/keys` prints, on the conversation.
    pub fn card_of_keys(&mut self) {
        self.say(Said::Keys);
    }

    /// One line of this build's own words.
    pub fn note(&mut self, said: &str) {
        self.say(Said::Note(said.to_string()));
    }

    /// A turn that failed, worded the way every other surface words it.
    pub fn fault(&mut self, error: &RpcError) {
        self.say(Said::Fault(Box::new(error.clone())));
    }

    /// Every key this screen answers, as rows to settle.
    ///
    /// A card on the transcript rather than a screen of its own, which is what
    /// `?` opens in every other full-screen view here: `?` is a character in
    /// the line being typed, so the way in is `/keys`, and a card that
    /// replaced the screen would take the conversation away from a reader who
    /// asked what a key does while reading it.
    ///
    /// The editing keys are on it because they are the ones nothing else says.
    /// A reader can see that Enter asks - the footer says so - and cannot see
    /// that alt-b walks back a word.
    fn keys(&self, cols: usize) -> Vec<String> {
        let rows: [super::keys::Row; 9] = [
            ("enter", "ask what is on the line"),
            (
                self.theme.glyph("↑ ↓", "up dn"),
                "the question before this one, and the one after",
            ),
            (
                "pgup pgdn",
                "a screenful back through the conversation, and on",
            ),
            ("tab shift-tab", "the next turn, and the turn before"),
            (
                "ctrl-n ctrl-p",
                "the next match of /find, and the one before",
            ),
            ("esc", "back to the foot, and out of a search"),
            (
                "left right home end",
                "move along the line; alt-b and alt-f by word",
            ),
            (
                "ctrl-w ctrl-u ctrl-k",
                "take out a word, the line before the cursor, the line after",
            ),
            ("ctrl-c ctrl-d", "leave; ctrl-d only on an empty line"),
        ];
        let label = rows
            .iter()
            .map(|(keys, _)| visible_width(keys))
            .max()
            .unwrap_or(0);

        let mut lines = vec![self.theme.paint(Role::Heading, "keys").to_string()];
        lines.extend(rows.iter().map(|(keys, does)| {
            let pad = " ".repeat(label - visible_width(keys) + 2);
            let does = truncate(does, cols.saturating_sub(label + 4), self.theme.charset());
            format!(
                "  {}{pad}{}",
                self.theme.paint(Role::Accent, keys),
                self.theme.paint(Role::Muted, &does)
            )
        }));
        lines
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
            Said::Keys => self.keys(cols),
            Said::Opening => self.opening(cols),
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
        self.turns.clear();
        self.back = 0;
        self.live = Live::new(self.theme, cols, &self.phase, self.think);
        self.live.whole(self.whole);
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
        self.live.whole(self.whole);
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
                false => {
                    self.remember(&said);
                    Act::Asked(said)
                }
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
            Key::Up => self.recall(true),
            Key::Down => self.recall(false),
            Key::PageUp => self.scroll(screenful as isize),
            Key::PageDown => self.scroll(-(screenful as isize)),
            Key::Ctrl('n') => self.walk(true),
            Key::Ctrl('p') => self.walk(false),
            Key::Tab => self.turn(true),
            Key::BackTab => self.turn(false),
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

    /// Keep a question, for the walk.
    ///
    /// The same question asked twice in a row is kept once: a reader pressing
    /// up expects the question before this one, not the same one again. The
    /// walk itself ends here too, because the line they were writing has just
    /// been sent and there is nothing of it to come back to.
    fn remember(&mut self, said: &str) {
        self.walking = None;
        if self.history.last().map(String::as_str) == Some(said) {
            return;
        }
        self.history.push(said.to_string());
    }

    /// Walk back through what this reader has asked, or forward again.
    ///
    /// Forward past the newest question puts back whatever was on the line
    /// when the walk started, which is the draft they had not finished.
    fn recall(&mut self, back: bool) {
        if self.history.is_empty() {
            return;
        }
        let (at, draft) = match self.walking.take() {
            Some((at, draft)) => (at, draft),
            None => (self.history.len(), self.line.text()),
        };
        let at = match back {
            true => at.saturating_sub(1),
            false => at + 1,
        };
        match self.history.get(at) {
            Some(said) => {
                self.line.put(said);
                self.walking = Some((at, draft));
            }
            // Past the newest: the reader is back at their own line.
            None => self.line.put(&draft),
        }
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
        // How many rows the prompt wants, before the body is measured: the
        // two share the screen, and the prompt is the one that must not be
        // cut - a reader cannot steer a caret onto a row that is not drawn.
        let most = PROMPT.min(rows.saturating_sub(CHROME - 1).max(1));
        let grown = self
            .line
            .rows(cols.saturating_sub(visible_width(MARKER)), most)
            .0
            .len();
        let body = rows.saturating_sub(CHROME + grown - 1);
        self.body = body;
        let block = &block[block.len().saturating_sub(body)..];

        let mut frame = Frame::new(cols, rows);
        frame.row(bar(
            cols,
            &self.theme.paint(Role::Heading, "tetanus").to_string(),
            &self.theme.paint(Role::Muted, &self.heading()).to_string(),
        ));
        frame.blank();
        for line in self.window(body - block.len()) {
            frame.row(line);
        }
        for line in block {
            frame.row(line);
        }
        // What is left goes above the rule, so a conversation that has just
        // started sits at the top of the screen rather than the middle.
        while frame.free() > 2 + grown {
            frame.blank();
        }
        frame.row(
            self.theme
                .paint(Role::Muted, &self.theme.glyph("─", "-").repeat(cols))
                .to_string(),
        );

        let label = visible_width(MARKER);
        // The prompt takes what it needs, up to `PROMPT`, and the transcript
        // above it gives up those rows: what is being written is what the
        // reader is looking at.
        let (typed, at) = self.line.rows(cols.saturating_sub(label), most);
        // Where the prompt actually starts, counted as the rows already
        // spent. On a terminal too short for the whole arrangement the frame
        // drops what does not fit - the footer first - so the prompt is not
        // always two rows off the bottom, and a caret placed as though it were
        // lands on the rule above it.
        let first = rows.saturating_sub(frame.free());
        for (number, row) in typed.iter().enumerate() {
            let marker = match number {
                0 => self.theme.paint(Role::Accent, MARKER).to_string(),
                _ => " ".repeat(label),
            };
            frame.row(format!("{marker}{row}"));
        }
        frame.row(self.footer(cols));
        // The row of the prompt the caret is on, counted from where the
        // prompt began - and only when that row was drawn at all. A terminal
        // with no room for a prompt has nowhere to put a caret, and one placed
        // anyway would sit on whatever the frame did have room for. A terminal
        // no columns wide draws nothing, so there is nothing to point at
        // either; terminals report that width while they are being resized.
        if first + at.0 < rows && cols > 0 {
            frame.cursor(first + at.0, label + at.1);
        }
        frame
    }

    /// The slice of the transcript this frame shows, `room` rows of it.
    fn window(&mut self, room: usize) -> &[String] {
        self.back = self.back.min(self.lines.len().saturating_sub(room));
        let end = self.lines.len() - self.back;
        &self.lines[end.saturating_sub(room)..end]
    }

    /// The page a conversation with nothing on it yet shows.
    ///
    /// A blank screen with a prompt on it is a screen that might be broken.
    /// The browser panel says `Nothing said yet. Ask something below.` for the
    /// same reason, and a terminal has the room to say the rest of what a
    /// reader needs: where the journal is, and that the commands exist.
    ///
    /// It is not on the transcript. Nothing was said, so there is nothing to
    /// scroll back to, and the first thing that is said takes the rows.
    fn opening(&self, cols: usize) -> Vec<String> {
        let said = [
            self.theme
                .paint(Role::Muted, "Nothing asked yet. Type a question below.")
                .to_string(),
            String::new(),
            format!(
                "{}  {}",
                self.theme.paint(Role::Muted, "journal"),
                truncate(&self.journal, cols.saturating_sub(11), self.theme.charset())
            ),
            format!(
                "{}  {}",
                self.theme.paint(Role::Muted, "keys   "),
                self.theme
                    .paint(Role::Muted, "/help for the commands, /keys for the keys")
            ),
        ];
        said.into_iter()
            .flat_map(|line| match visible_width(&line) + 2 > cols {
                // Only the last row can be too wide, and only on a terminal
                // narrow enough that the sentence has to fold. A path is the
                // one thing here that is cut instead: half a path is not a
                // path, and it has a column of its own to be cut to.
                true => wrap(&plain(&line), cols.saturating_sub(2)),
                false => vec![line],
            })
            .map(|line| format!("  {line}"))
            .collect()
    }

    /// What the heading says on the right: the model, and how much
    /// conversation there is.
    ///
    /// The count is the one fact about a resumed chat that is nowhere else on
    /// the screen once its opening has scrolled away, and it is what tells a
    /// reader whether the journal they named is the one they meant.
    fn heading(&self) -> String {
        match self.turns.len() {
            0 => self.title.clone(),
            1 => format!("{} {} 1 turn", self.title, self.theme.glyph("·", "-")),
            turns => format!(
                "{} {} {turns} turns",
                self.title,
                self.theme.glyph("·", "-")
            ),
        }
    }

    /// The keys on the left, where the reader is on the right.
    fn footer(&self, cols: usize) -> String {
        let dot = self.theme.glyph("·", "-");
        let full = match self.wanted.is_some() {
            // The keys that matter while a search is up are the ones that walk
            // it, and they are not on the footer the rest of the time.
            true => format!("ctrl-n next {dot} ctrl-p back {dot} esc done {dot} ctrl-c leave"),
            false => format!(
                "enter ask {dot} {} history {dot} pgup back {dot} tab turn {dot} ctrl-c leave",
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
/// there are none, and that ctrl-n and ctrl-p walk them and go round; that Tab
/// and Shift-Tab walk the turns and land on either end of the conversation; that every printable key belongs to the line being typed and the keys
/// the editor does not answer move the window; that Enter asks and an empty
/// line does not; that a line typed while a turn is running is held rather
/// than sent; that Ctrl-C and Ctrl-D are told apart; that arriving lines do
/// not move a window a reader has scrolled back; that a resize composes the
/// conversation again and a search open at the time survives it; and that a
/// line longer than the row keeps the cursor on the row; and that the screen
/// carries a rule between what was said and what is being typed, and a
/// heading that counts the turns; and that `/keys` names every key the screen
/// answers, the editing ones included, on the conversation rather than over
/// it; and that the up and down keys walk what this reader has asked, keep
/// the draft they were writing, and keep a repeated question once; and that a
/// conversation with nothing in it says so and names its journal, until the
/// first turn answers it; and that `/more` and `/think` open what is already
/// on the page, both ways; and that the prompt grows for a question longer
/// than a row, up to a bound, with the transcript giving up those rows.
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
        Fire::new(theme(), COLS, "mock-echo-1", "sessions/chat.jsonl", false)
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

    /// The event a turn opens on.
    fn turn_start(turn: u32) -> SessionEvent {
        SessionEvent {
            ty: "turn/start".into(),
            seq: u64::from(turn),
            time: 0,
            data: serde_json::json!({ "turn": turn }),
            source_event_seqs: None,
        }
    }

    /// A message the model reasoned before it wrote.
    fn thought(reasoning: &str, content: &str) -> SessionEvent {
        SessionEvent {
            ty: "assistant/message".into(),
            seq: 2,
            time: 0,
            data: serde_json::json!({
                "content": content,
                "reasoning": reasoning,
                "turn": 1,
                "step": 1,
            }),
            source_event_seqs: None,
        }
    }

    /// A tool result carrying `content`.
    fn produced(content: &str) -> SessionEvent {
        SessionEvent {
            ty: "tool/result".into(),
            seq: 3,
            time: 0,
            data: serde_json::json!({
                "call_id": "c1",
                "name": "echo",
                "ok": true,
                "content": content,
                "turn": 1,
                "step": 1,
            }),
            source_event_seqs: None,
        }
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

        // The page keys are what scrolls: the arrows walk the history, the
        // way they do at every other prompt.
        assert_eq!(view.key(Key::PageUp), Act::Go);
        let back = rows(&mut view, ROWS);
        assert!(back[ROWS - 1].contains(" back"), "{back:?}");

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
        let mut view = Fire::new(theme(), 100, "mock-echo-1", "sessions/chat.jsonl", false);
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
        let mut view = Fire::new(theme(), 90, "mock-echo-1", "sessions/chat.jsonl", false);
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
        let mut view = Fire::new(theme(), 90, "mock-echo-1", "sessions/chat.jsonl", false);
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

    /// TC-CLI-FIRE-12: Tab and Shift-Tab over a conversation of three turns.
    /// Expected: the window lands with a turn's own opening line at the top of
    /// the body, one turn at a time, in the direction asked for. This is the
    /// terminal's answer to the message list the web panel puts beside a
    /// conversation: what a reader reaches for is the start of a turn, and
    /// there is no list to click.
    #[test]
    fn tab_walks_the_turns_and_shift_tab_walks_them_back() {
        let mut view = fire();
        for turn in 1..=3 {
            view.push(&turn_start(turn));
            for line in 1..=6 {
                view.note(&format!("turn {turn} line {line}"));
            }
        }
        rows(&mut view, ROWS);

        // The body opens at row two, and a turn opens on the blank row that
        // separates it from the turn before, so the header is one of the two
        // rows the jump puts at the top.
        let opens_on =
            |rows: &[String], turn: &str| rows[2..4].iter().any(|row| row.contains(turn));

        // From the foot, back one turn at a time.
        assert_eq!(view.key(Key::BackTab), Act::Go);
        let third = rows(&mut view, ROWS);
        assert!(opens_on(&third, "turn 3"), "not the third turn: {third:?}");

        assert_eq!(view.key(Key::BackTab), Act::Go);
        let second = rows(&mut view, ROWS);
        assert!(opens_on(&second, "turn 2"), "not the second: {second:?}");

        assert_eq!(view.key(Key::Tab), Act::Go);
        let forward = rows(&mut view, ROWS);
        assert!(opens_on(&forward, "turn 3"), "not forwards: {forward:?}");
    }

    /// TC-CLI-FIRE-13: Tab past the last turn, and Shift-Tab before the first.
    /// Expected: the foot of the conversation, and the top of it. A reader
    /// walking forwards is heading for the end, and one walking back is
    /// heading for the beginning; neither end is a refusal to move.
    #[test]
    fn walking_past_either_end_lands_on_it() {
        let mut view = fire();
        view.push(&turn_start(1));
        for line in 1..=20 {
            view.note(&format!("line {line}"));
        }
        rows(&mut view, ROWS);

        view.key(Key::Tab);
        assert!(
            rows(&mut view, ROWS)[ROWS - 1].contains("end"),
            "forwards did not reach the foot"
        );

        view.key(Key::BackTab);
        view.key(Key::BackTab);
        let top = rows(&mut view, ROWS);
        assert!(
            top.iter().any(|row| row.contains("turn 1")),
            "back did not reach the first turn: {top:?}"
        );
    }

    /// TC-CLI-FIRE-14: the rule between the conversation and the composer.
    /// Expected: one row of the charset's own line, the full width, directly
    /// above the prompt. What a reader is typing has not been said yet, and
    /// without a line between them a half-written question reads as the newest
    /// thing on the transcript.
    #[test]
    fn a_rule_separates_what_was_said_from_what_is_being_typed() {
        let mut view = fire();
        view.note("something said");
        typing(&mut view, "half a question");

        let drawn = rows(&mut view, ROWS);
        assert_eq!(drawn[ROWS - 3], "─".repeat(COLS), "no rule: {drawn:?}");
        assert_eq!(drawn[ROWS - 2], "> half a question", "{drawn:?}");

        // A terminal that cannot draw the line gets the one it can.
        let mut plain = Fire::new(
            Theme::new(false, Charset::Ascii),
            COLS,
            "m",
            "j.jsonl",
            false,
        );
        plain.note("something said");
        let drawn = rows_at(&mut plain, COLS, ROWS);
        assert_eq!(drawn[ROWS - 3], "-".repeat(COLS), "{drawn:?}");
    }

    /// TC-CLI-FIRE-15: the heading, on a conversation of none, one and three
    /// turns.
    /// Expected: the model alone, then the model and the count, singular and
    /// plural. Once the opening page has scrolled away the count is the one
    /// fact about a resumed journal that is nowhere else on the screen, and it
    /// is what tells a reader whether the journal they named is the one they
    /// meant.
    #[test]
    fn the_heading_says_how_much_conversation_there_is() {
        let mut view = fire();
        assert!(rows(&mut view, ROWS)[0].ends_with("chat on mock-echo-1"));

        view.push(&turn_start(1));
        assert!(rows(&mut view, ROWS)[0].ends_with("mock-echo-1 · 1 turn"));

        view.push(&turn_start(2));
        view.push(&turn_start(3));
        assert!(rows(&mut view, ROWS)[0].ends_with("mock-echo-1 · 3 turns"));
    }

    /// TC-CLI-FIRE-16: the card `/keys` prints.
    /// Expected: a heading, one row per key, the descriptions in a column of
    /// their own, and nothing over the window - and the editing keys among
    /// them, because those are the ones nothing else on the screen says. The
    /// footer says Enter asks; nothing says alt-b walks back a word.
    #[test]
    fn the_keys_card_names_the_editing_keys_too() {
        let mut view = fire();
        view.card_of_keys();

        let drawn = rows(&mut view, ROWS + 8);
        for row in &drawn {
            assert!(visible_width(row) <= COLS, "`{row}` overruns {COLS}");
        }
        assert!(drawn.iter().any(|row| row.trim() == "keys"), "{drawn:?}");
        for key in ["enter", "tab shift-tab", "ctrl-n ctrl-p", "alt-b", "ctrl-w"] {
            assert!(
                drawn.iter().any(|row| row.contains(key)),
                "`{key}` is on no row: {drawn:?}"
            );
        }

        // The card is on the conversation, not over it: the reader asked what
        // a key does while reading something, and it is still there.
        view.note("said before the card");
        let after = rows(&mut view, ROWS + 8);
        assert!(
            after.iter().any(|row| row.contains("said before the card")),
            "{after:?}"
        );
    }

    /// TC-CLI-FIRE-17: the up and down keys, over three questions and a draft.
    /// Expected: up walks back through what this reader asked, down walks
    /// forward again, and forward past the newest puts back the half-written
    /// line the walk started from. A reader who pressed up to check what they
    /// asked last time has not thrown away the question they were writing.
    #[test]
    fn the_arrows_walk_the_history_and_give_the_draft_back() {
        let mut view = fire();
        for said in ["first", "second", "third"] {
            typing(&mut view, said);
            assert_eq!(view.key(Key::Enter), Act::Asked(said.into()));
        }
        typing(&mut view, "half a th");

        let on_the_row = |view: &mut Fire| rows(view, ROWS)[ROWS - 2].clone();

        view.key(Key::Up);
        assert_eq!(on_the_row(&mut view), "> third");
        view.key(Key::Up);
        view.key(Key::Up);
        assert_eq!(on_the_row(&mut view), "> first");
        // The oldest is the oldest: pressing up again does not walk off it.
        view.key(Key::Up);
        assert_eq!(on_the_row(&mut view), "> first");

        view.key(Key::Down);
        assert_eq!(on_the_row(&mut view), "> second");
        view.key(Key::Down);
        view.key(Key::Down);
        assert_eq!(on_the_row(&mut view), "> half a th", "the draft was lost");
    }

    /// TC-CLI-FIRE-18: a recalled question, edited and asked again, and the
    /// same question asked twice.
    /// Expected: the edited one is what is sent and what the history keeps,
    /// and a question repeated is kept once - a reader pressing up expects the
    /// question before this one, not the same one again.
    #[test]
    fn the_history_keeps_what_was_asked_and_not_what_was_recalled() {
        let mut view = fire();
        typing(&mut view, "count the files");
        view.key(Key::Enter);

        view.key(Key::Up);
        typing(&mut view, " twice");
        assert_eq!(
            view.key(Key::Enter),
            Act::Asked("count the files twice".into())
        );

        view.key(Key::Up);
        assert_eq!(rows(&mut view, ROWS)[ROWS - 2], "> count the files twice");

        // Asked again, unchanged: one entry, not two.
        view.key(Key::Enter);
        view.key(Key::Up);
        view.key(Key::Up);
        assert_eq!(rows(&mut view, ROWS)[ROWS - 2], "> count the files");
    }

    /// TC-CLI-FIRE-19: the page a conversation with nothing in it opens on,
    /// and what becomes of it.
    /// Expected: it says nothing has been asked, names the journal and the two
    /// commands, and it is gone the moment a turn exists - a blank screen with
    /// a prompt on it is a screen that might be broken, and a page still
    /// saying "nothing asked yet" above an answer is worse than one that was
    /// never there.
    #[test]
    fn a_conversation_with_nothing_in_it_says_where_it_is() {
        let mut view = fire();
        let opened = rows(&mut view, ROWS);
        assert!(
            opened.iter().any(|row| row.contains("Nothing asked yet")),
            "{opened:?}"
        );
        assert!(
            opened.iter().any(|row| row.contains("sessions/chat.jsonl")),
            "the journal is not named: {opened:?}"
        );
        assert!(opened.iter().any(|row| row.contains("/keys")), "{opened:?}");

        view.push(&turn_start(1));
        let asked = rows(&mut view, ROWS);
        assert!(
            !asked.iter().any(|row| row.contains("Nothing asked yet")),
            "the opening page outlived the first turn: {asked:?}"
        );
    }

    /// TC-CLI-FIRE-20: the opening page on a window too narrow for it.
    /// Expected: it folds rather than overrunning, and the journal - the one
    /// thing on it that is a value rather than a sentence - is cut to its own
    /// column instead, because half a path is not a path.
    #[test]
    fn the_opening_page_folds_where_the_window_is_narrow() {
        let mut view = Fire::new(
            theme(),
            34,
            "mock-echo-1",
            "sessions/a/very/long/chat.jsonl",
            false,
        );

        for row in rows_at(&mut view, 34, ROWS + 4) {
            assert!(visible_width(&row) <= 34, "`{row}` overruns 34");
        }
    }

    /// TC-CLI-FIRE-21: `/more` over a tool result longer than the cap, and
    /// `/think` over a message that reasoned before it answered.
    /// Expected: both change what is already on the page, both ways, because
    /// they are a reader changing their mind about what they are looking at
    /// rather than a flag they had to know about before they started. The
    /// browser panel opens the same card; this is the terminal's way of
    /// opening it.
    #[test]
    fn more_and_think_open_what_is_already_on_the_page() {
        let mut view = fire();
        view.push(&turn_start(1));
        view.push(&thought("first thought\nsecond thought", "the answer"));
        view.push(&produced(
            &(1..=40)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));

        let capped = rows(&mut view, ROWS + 30);
        assert!(
            capped.iter().any(|row| row.contains("+24 lines")),
            "the result is not capped: {capped:?}"
        );
        assert!(
            !capped.iter().any(|row| row.contains("line 20")),
            "a capped result is showing its middle: {capped:?}"
        );
        assert!(
            !capped.iter().any(|row| row.contains("second thought")),
            "the thinking is not folded: {capped:?}"
        );

        assert!(view.whole(), "/more did not turn on");
        let whole = rows(&mut view, ROWS + 40);
        assert!(
            whole.iter().any(|row| row.contains("line 20")),
            "the middle is still hidden: {whole:?}"
        );

        assert!(view.thinking(), "/think did not turn on");
        assert!(
            rows(&mut view, ROWS + 40)
                .iter()
                .any(|row| row.contains("second thought")),
            "the thinking is still folded"
        );

        // And back: both are toggles, because a reader who opened one to look
        // at something wants their page back afterwards.
        assert!(!view.whole());
        assert!(!view.thinking());
        let folded = rows(&mut view, ROWS + 30);
        assert!(
            !folded.iter().any(|row| row.contains("line 20")),
            "{folded:?}"
        );
        assert!(
            !folded.iter().any(|row| row.contains("thought hard")),
            "{folded:?}"
        );
    }

    /// TC-CLI-FIRE-22: a question longer than one row of the prompt.
    /// Expected: the prompt grows, the transcript gives up the rows, the caret
    /// stays on the row the last character is on, and the growth stops at
    /// `PROMPT` rows - after which the prompt scrolls inside them the way a
    /// single row scrolled sideways. Every panel with a text box grows it,
    /// this project's own included; a reader cannot check the sentence they
    /// are writing if the prompt only ever shows its tail.
    #[test]
    fn the_prompt_grows_for_a_question_longer_than_a_row() {
        let mut view = fire();
        for n in 1..=12 {
            view.note(&format!("line {n}"));
        }
        let one = rows(&mut view, ROWS);
        assert_eq!(one[ROWS - 2], ">", "the prompt is not one row: {one:?}");

        typing(&mut view, &"x".repeat((COLS - 2) * 2 + 4));
        let grown = rows(&mut view, ROWS);
        assert_eq!(grown.len(), ROWS, "the frame changed height");
        assert!(grown[ROWS - 4].starts_with("> x"), "{grown:?}");
        assert!(grown[ROWS - 3].starts_with("  x"), "{grown:?}");
        assert!(grown[ROWS - 2].starts_with("  x"), "{grown:?}");
        assert_eq!(
            cursor(&mut view, ROWS).0,
            ROWS - 2,
            "the caret left the row"
        );

        // The transcript gave up exactly the rows the prompt took.
        let shown = |rows: &[String]| rows.iter().filter(|row| row.starts_with("line ")).count();
        assert_eq!(
            shown(&one) - shown(&grown),
            2,
            "the transcript did not give up two rows: {grown:?}"
        );

        // And it stops growing: a very long line keeps the prompt at PROMPT
        // rows and scrolls inside them.
        typing(&mut view, &"y".repeat(COLS * 6));
        let capped = rows(&mut view, ROWS + 6);
        let prompt = capped
            .iter()
            .filter(|row| row.contains('y') || row.starts_with("> "))
            .count();
        assert!(
            prompt <= PROMPT,
            "the prompt grew past {PROMPT}: {capped:?}"
        );
    }

    /// TC-CLI-FIRE-23: every size a terminal can be, including the ones it
    /// should not be.
    /// Expected: the frame is exactly the height asked for, no row overruns
    /// the width, and the caret is inside the frame - at no columns by no rows
    /// as much as at two hundred by sixty. A terminal reports zero while it is
    /// being resized, and a view that panicked there would take the
    /// conversation with it. Six slices have moved rows
    /// between the transcript, the block, the opening page and a prompt that
    /// grows, and each of them is arithmetic that has to hold at both ends.
    #[test]
    fn the_arrangement_holds_at_every_size() {
        for rows in 0..=10 {
            for cols in 0..=24 {
                for working in [false, true] {
                    let mut view = Fire::new(theme(), cols, "mock-echo-1", "j.jsonl", false);
                    view.note("something said before");
                    if working {
                        view.started("running the turn");
                        view.push(&said_by("you", "a question of some length"));
                    }
                    typing(&mut view, "a question long enough to fold");

                    let frame = view.frame(cols, rows, Duration::from_secs(1));
                    assert_eq!(frame.rows(), rows, "{cols}x{rows} working={working}");

                    let mut ui = buffered(theme(), cols);
                    frame.paint(&mut ui).expect("paint");
                    let painted = ui.contents();
                    for row in painted
                        .trim_start_matches("\x1b[H")
                        .split("\r\n")
                        .map(|row| row.split('\x1b').next().unwrap_or_default())
                    {
                        assert!(
                            visible_width(row) <= cols,
                            "`{row}` overruns {cols} at {cols}x{rows}"
                        );
                    }
                    let drawn: Vec<&str> = painted
                        .trim_start_matches("\x1b[H")
                        .split("\r\n")
                        .map(|row| row.split('\x1b').next().unwrap_or_default())
                        .collect();
                    // A caret exactly when there is a prompt to point at: the
                    // arrangement spends its first rows on the heading, the
                    // blank and the rule, so the prompt is the fourth - and a
                    // terminal no columns wide draws nothing to point at.
                    let shown = painted.ends_with("\x1b[?25h");
                    let prompted = cols > 0 && rows >= CHROME - 1;
                    assert_eq!(
                        shown, prompted,
                        "caret and prompt disagree at {cols}x{rows}"
                    );
                    if !shown {
                        continue;
                    }
                    let at = painted.rfind('H').expect("a cursor move");
                    let place = painted[..at].rsplit_once("\x1b[").expect("a move").1;
                    let (row, col) = place.split_once(';').expect("row;col");
                    let (row, col): (usize, usize) =
                        (row.parse().expect("row"), col.parse().expect("col"));
                    assert!(
                        row <= rows && col <= cols,
                        "the caret is off the screen at {cols}x{rows}: {row};{col}"
                    );
                    // And on the prompt, not on whatever the frame had room
                    // for above it: a terminal too short for the arrangement
                    // drops the footer, and a caret placed as though the
                    // footer were always there lands on the rule. The rule is
                    // the row before the prompt, so a caret on or above it is
                    // the bug this case was written for.
                    assert!(
                        row >= CHROME - 1,
                        "the caret is above the prompt at {cols}x{rows}: row {row}"
                    );
                    let on = drawn.get(row - 1).copied().unwrap_or_default();
                    assert!(
                        !on.starts_with('─') && !on.starts_with('-'),
                        "the caret is on the rule at {cols}x{rows}: `{on}`"
                    );
                }
            }
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
