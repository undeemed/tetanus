//! A finished journal, read back on a screen of its own.
//!
//! `tetanus replay <path>` prints a whole turn into the scrollback, which is
//! the right answer for a short journal and the wrong one for a long. A turn
//! with sixty tool calls in it arrives as a wall the reader then scrolls their
//! shell back through, mixed in with whatever was on the screen before. This
//! module is the other view of the same lines: the alternate screen, one page
//! at a time, with keys to move through it, and the scrollback left exactly as
//! it was found.
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
//! # Rationale: a resize refills the page
//!
//! A [`Page`] never rewraps a line it has been given, because a live view must
//! not rewrite history under a reader who is still reading it. A finished
//! journal is not under that constraint: its lines can simply be composed
//! again at the new width, and not doing so would leave a page the reader
//! widened still wrapped for the screen they widened it from. So a resize
//! refills, keeping how far back the reader had scrolled. That distance is in
//! rows and rows mean something slightly different after a rewrap, which is
//! the price of the alternative being visibly wrong.

use std::io::{self, Write};
use std::time::Duration;

use tetanus_protocol::types::SessionEvent;
use tetanus_ui::{show, size, Flow, Frame, Key, Page, Show, Stop, Theme, Tty, Ui, View};

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
    let keys = format!(
        "{} scroll {} q quit",
        theme.glyph("↑↓", "up/dn"),
        theme.glyph("·", "-")
    );
    let mut journal = Journal::new(theme, title, events.to_vec(), think, keys, (cols, rows));
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
    keys: String,
    /// The width the page was last filled at.
    cols: usize,
    /// The height of the last frame, which is what a PageUp is measured in.
    rows: usize,
}

impl Journal {
    /// A journal filled and ready to draw at `size`.
    pub(super) fn new(
        theme: Theme,
        title: &str,
        events: Vec<SessionEvent>,
        think: bool,
        keys: String,
        size: (usize, usize),
    ) -> Self {
        let mut journal = Self {
            events,
            theme,
            think,
            title: title.to_string(),
            page: Page::new(theme, NAME, title),
            keys,
            // Not `size.0`: the fill below is what makes the page true at a
            // width, and starting them equal would claim it already had.
            cols: 0,
            rows: size.1,
        };
        journal.fill(size.0);
        journal
    }

    /// Compose every line again at `cols`, keeping how far back the reader is.
    fn fill(&mut self, cols: usize) {
        let back = self.page.back();
        let mut page = Page::new(self.theme, NAME, &self.title);
        let mut reader = Reader::new(self.think);
        for event in &self.events {
            page.settle(reader.lines(&self.theme, cols, event));
        }
        page.scroll(isize::try_from(back).unwrap_or(isize::MAX));
        self.page = page;
        self.cols = cols;
    }
}

impl View for Journal {
    fn frame(&mut self, cols: usize, rows: usize) -> Frame {
        if cols != self.cols {
            self.fill(cols);
        }
        self.rows = rows;
        // No block: a journal on disk has nothing arriving, which is what the
        // footer reads back as `end` rather than `live`.
        self.page.frame(cols, rows, &[], &self.keys)
    }

    fn key(&mut self, key: Key) -> Flow {
        let screenful = isize::try_from(self.rows.saturating_sub(KEPT).max(1)).unwrap_or(1);
        match key {
            Key::Char('q') | Key::Esc => return Flow::Stop,
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
/// the new width without losing the reader's place; and the key map, including
/// the two keys that end the view.
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

    fn journal(events: &[SessionEvent], cols: usize) -> Journal {
        Journal::new(
            theme(),
            "j.jsonl",
            events.to_vec(),
            false,
            "up/dn scroll".into(),
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
}
