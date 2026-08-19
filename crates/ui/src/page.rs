//! A frame with a transcript in it, and a way back through the transcript.
//!
//! [`Screen`](crate::Screen) owns a block on the ordinary screen and lets the
//! lines above it scroll away into the terminal's own scrollback. On the
//! alternate screen there is no scrollback to scroll into: the canvas is the
//! whole terminal, and a line that leaves the top of it is gone. So a
//! full-screen view has to keep the transcript itself, and give the reader a
//! way back through it.
//!
//! That is what a page is. [`Frame`] is the canvas and knows nothing about
//! what goes on it; a page is the arrangement almost every full-screen view of
//! a stream wants:
//!
//! - **The transcript is kept.** Nothing is ever dropped, and each frame shows
//!   the window of it that fits, so a reader can go back to the first line of
//!   something that has been running for an hour.
//! - **The block is pinned** to the foot of the body. It is what is happening
//!   now, and scrolling it away would leave a reader looking back with no way
//!   to tell work in progress from work that has stopped.
//! - **Arriving lines do not move the window.** A reader who has scrolled back
//!   keeps the lines under their eye while new ones settle underneath. The
//!   alternative drags the page out from under them, one row per arriving
//!   line, which is the fastest way to make a view unreadable.
//!
//! # Composition
//!
//! ```text
//! tetanus                                    mock deepseek-chat   heading
//!                                                                 blank
//! turn 1                                                       |
//!   step 1                                                     |  transcript
//!   you  run one full turn                                     |
//!   ai   the answer as the chunks assemble it                  |  block
//!   * streaming the answer - 1.2s                              |
//!                                                                 blank
//! up/dn scroll - q quit                                 live      footer
//! ```
//!
//! Nothing here writes, reads a clock, or asks the terminal anything. The
//! caller owns all three: it settles the lines, hands over the block, and
//! states the size. So a page is a pure function of what it was given, and
//! every case in the suite says exactly what it gave.

use crate::frame::Frame;
use crate::text::visible_width;
use crate::theme::{Role, Theme};

/// Rows a frame spends on furniture: the heading, a blank under it, a blank
/// over the footer, and the footer. Everything left is the body.
const CHROME: usize = 4;

/// A transcript, a block, and the furniture around them.
pub struct Page {
    theme: Theme,
    /// The left of the heading: what the reader is looking at. Usually the
    /// name of the program.
    name: String,
    /// The right of the heading: which one of them they are looking at.
    title: String,
    /// Every settled line, oldest first.
    lines: Vec<String>,
    /// How many rows above the newest one the window ends. Zero follows the
    /// stream; anything else is a reader who has scrolled back.
    back: usize,
}

impl Page {
    /// An empty page, headed `name` on the left and `title` on the right.
    pub fn new(theme: Theme, name: &str, title: &str) -> Self {
        Self {
            theme,
            name: name.to_string(),
            title: title.to_string(),
            lines: Vec::new(),
            back: 0,
        }
    }

    /// Add lines to the transcript. Settled means final: a page never rewrites
    /// a line it has been given, and never rewraps one, because rewrapping is
    /// rewriting history the reader has already read.
    pub fn settle(&mut self, lines: Vec<String>) {
        if self.back > 0 {
            self.back += lines.len();
        }
        self.lines.extend(lines);
    }

    /// Move the window `rows` further back through the transcript. A negative
    /// number moves it toward the newest line, and neither end overshoots.
    pub fn scroll(&mut self, rows: isize) {
        // The far end is clamped in `window`, where the height of the terminal
        // is known. Here there is only the near one.
        self.back = self.back.saturating_add_signed(rows);
    }

    /// Follow the newest line again.
    pub fn follow(&mut self) {
        self.back = 0;
    }

    /// How far back the window is, in rows. Zero while it is following.
    pub fn back(&self) -> usize {
        self.back
    }

    /// The whole screen as of now: `block` at the foot of the body, `keys` at
    /// the left of the footer.
    ///
    /// An empty `block` reads as a stream that has ended, which is what the
    /// right of the footer says when the window is following it.
    ///
    /// The size comes from the caller on every call rather than being kept,
    /// because a frame is built for the size the terminal has at the moment it
    /// is painted, and there is nothing to reflow because nothing was kept.
    pub fn frame(&mut self, cols: usize, rows: usize, block: &[String], keys: &str) -> Frame {
        let body = rows.saturating_sub(CHROME);
        // Asked before the fit below, which empties a block on a terminal too
        // small to hold one. An empty block means the stream has ended, and a
        // view that lost the room to say so has not stopped being live.
        let over = block.is_empty();
        // A block taller than the body would fill the frame and push the
        // footer off it. Its last row is the one that has to survive, so the
        // window into it starts from the end.
        let block = &block[block.len().saturating_sub(body)..];

        let mut frame = Frame::new(cols, rows);
        frame.row(bar(
            cols,
            &self.theme.paint(Role::Heading, &self.name).to_string(),
            &self.theme.paint(Role::Muted, &self.title).to_string(),
        ));
        frame.blank();
        for line in self.window(body - block.len()) {
            frame.row(line);
        }
        for line in block {
            frame.row(line);
        }
        // Whatever is left over goes between the transcript and the footer, so
        // a short stream sits at the top of the screen rather than the middle.
        while frame.free() > 1 {
            frame.blank();
        }
        frame.row(self.footer(cols, keys, over));
        frame
    }

    /// The slice of the transcript this frame shows, `room` rows of it.
    fn window(&mut self, room: usize) -> &[String] {
        // How far back there is to go depends on the height of the terminal,
        // which is known here and nowhere else, so the clamp lives here. It
        // sticks, so a reader who scrolled past the start of a short transcript
        // and then made the window taller is not left staring at blank rows.
        self.back = self.back.min(self.lines.len().saturating_sub(room));
        let end = self.lines.len() - self.back;
        &self.lines[end.saturating_sub(room)..end]
    }

    /// The caller's keys on the left, where the reader is on the right.
    fn footer(&self, cols: usize, keys: &str, over: bool) -> String {
        let here = match (self.back, over) {
            (0, true) => "end".to_string(),
            (0, false) => "live".to_string(),
            (back, _) => format!("{back} back"),
        };
        bar(
            cols,
            &self.theme.paint(Role::Muted, keys).to_string(),
            &self.theme.paint(Role::Accent, &here).to_string(),
        )
    }
}

/// One row with something at each end.
///
/// Measured by what a terminal draws, not by bytes, so the SGR codes a theme
/// wrote do not push the right end off the screen.
fn bar(cols: usize, left: &str, right: &str) -> String {
    let room = visible_width(left) + visible_width(right) + 1;
    match cols.checked_sub(room) {
        Some(gap) => format!("{left}{}{right}", " ".repeat(gap + 1)),
        // Too narrow for both. The left end says what this is and the right
        // end says where in it you are; the first is the one worth keeping,
        // and `Frame` cuts it to the width on the way out.
        None => left.to_string(),
    }
}

/// Test Design Specification: the scrollable page.
///
/// Features tested: that a frame is exactly the screen it was asked for with
/// the furniture in its places; that a transcript taller than the screen shows
/// its tail; that a scroll holds the reader's lines still while new ones
/// settle; that neither end of the scroll overshoots and `follow` comes back;
/// that the block stays pinned to the foot of the body whatever the scroll
/// does; and that a screen too small for the furniture composes rather than
/// underflowing.
///
/// Features NOT tested here: painting and the exact height of a frame (owned
/// by `frame.rs`), the cut to the width (owned by `text::fit`), the colour
/// policy (owned by `theme.rs`), and where the lines came from - a page is
/// given them.
///
/// Environmental needs: none. Every case composes into a buffer at a size it
/// states.
#[cfg(test)]
mod tests {
    use crate::color::Charset;
    use crate::writer::buffered;

    use super::*;

    const COLS: usize = 40;
    const KEYS: &str = "up/dn scroll";

    fn theme() -> Theme {
        Theme::new(false, Charset::Unicode)
    }

    fn page(lines: usize) -> Page {
        let mut page = Page::new(theme(), "tetanus", "mock deepseek-chat");
        page.settle((1..=lines).map(|line| format!("line {line}")).collect());
        page
    }

    /// The rows a frame paints, with the codes that position them taken back
    /// off. `Frame` keeps its rows to itself, and reading them back through
    /// the paint is what proves the frame a user sees is this one.
    fn rows(frame: &Frame) -> Vec<String> {
        let mut ui = buffered(theme(), frame.cols());
        frame.paint(&mut ui).expect("paint");
        ui.contents()
            .trim_start_matches("\x1b[H")
            .trim_end_matches("\x1b[J")
            .split("\r\n")
            .map(|row| row.trim_end_matches("\x1b[K").to_string())
            .collect()
    }

    /// TC-UI-PAGE-1: an empty page.
    /// Expected: exactly the rows asked for; the heading naming both ends; the
    /// block at the foot of the body; the window following, so the right of
    /// the footer says `live`.
    #[test]
    fn a_frame_is_the_screen_it_was_asked_for() {
        let block = vec!["working".to_string()];
        let told = rows(&page(0).frame(COLS, 10, &block, KEYS));

        assert_eq!(told.len(), 10);
        assert!(told[0].starts_with("tetanus"), "{:?}", told[0]);
        assert!(told[0].ends_with("mock deepseek-chat"), "{:?}", told[0]);
        assert_eq!(told[1], "");
        assert_eq!(told[2], "working");
        assert_eq!(told[8], "");
        assert!(told[9].starts_with(KEYS), "{:?}", told[9]);
        assert!(told[9].ends_with("live"), "{:?}", told[9]);
    }

    /// TC-UI-PAGE-2: more transcript than screen.
    /// Expected: the newest lines, not the oldest, and the block under them. A
    /// view that showed the top of a long stream and stopped would hide the
    /// part still arriving.
    #[test]
    fn a_transcript_taller_than_the_screen_shows_its_tail() {
        let block = vec!["working".to_string()];
        let told = rows(&page(20).frame(COLS, 10, &block, KEYS));

        // Ten rows, less the four of furniture and the one the block takes.
        assert_eq!(told[2], "line 16");
        assert_eq!(told[6], "line 20");
        assert_eq!(told[7], "working");
    }

    /// TC-UI-PAGE-3: reading the start of a stream that is still going.
    /// Expected: the lines under the reader's eye do not move when new ones
    /// settle, and the footer counts how far back they are. This is the whole
    /// reason a page keeps a transcript rather than a window.
    #[test]
    fn arriving_lines_do_not_drag_the_page_out_from_under_a_reader() {
        let block = vec!["working".to_string()];
        let mut page = page(20);
        page.scroll(10);
        let before = rows(&page.frame(COLS, 10, &block, KEYS));
        page.settle((21..=25).map(|line| format!("line {line}")).collect());
        let after = rows(&page.frame(COLS, 10, &block, KEYS));

        assert_eq!(before[2..7], after[2..7]);
        assert!(before[9].ends_with("10 back"), "{:?}", before[9]);
        assert!(after[9].ends_with("15 back"), "{:?}", after[9]);
    }

    /// TC-UI-PAGE-4: scrolling past either end.
    /// Expected: the first line stays on screen however far back the reader
    /// goes, the newest however far forward, and `follow` gets there in one
    /// step. A window that left the transcript would answer a held key with a
    /// blank screen.
    #[test]
    fn neither_end_of_the_transcript_overshoots() {
        let block = vec!["working".to_string()];
        let mut page = page(20);

        page.scroll(500);
        assert_eq!(rows(&page.frame(COLS, 10, &block, KEYS))[2], "line 1");

        page.scroll(-500);
        let bottom = rows(&page.frame(COLS, 10, &block, KEYS));
        assert_eq!(bottom[6], "line 20");
        assert_eq!(page.back(), 0);

        page.scroll(3);
        page.follow();
        assert_eq!(rows(&page.frame(COLS, 10, &block, KEYS)), bottom);
    }

    /// TC-UI-PAGE-5: the block while the reader is somewhere else.
    /// Expected: the transcript scrolls and the block does not, however tall
    /// the block is. It is what is happening now; scrolling it away would
    /// leave a reader looking back with no way to tell a working stream from a
    /// stopped one.
    #[test]
    fn the_block_stays_pinned_to_the_foot_of_the_body() {
        let block = vec!["arriving".to_string(), "working".to_string()];
        let mut page = page(20);
        page.scroll(8);
        let told = rows(&page.frame(COLS, 12, &block, KEYS));

        assert_eq!(told[2], "line 7");
        assert_eq!(told[8], "arriving");
        assert_eq!(told[9], "working");
        assert!(told[11].ends_with("8 back"), "{:?}", told[11]);
    }

    /// TC-UI-PAGE-6: a window dragged down to nothing.
    /// Expected: a frame of the size asked for, and no panic. The arithmetic
    /// takes the furniture off the height and the block off what is left, and
    /// both go negative on a screen this small. A frame of no rows paints the
    /// two codes that home the cursor and erase, which read back as one empty
    /// row rather than none.
    #[test]
    fn a_screen_with_no_room_for_the_furniture_still_composes() {
        let block = vec!["arriving".to_string(), "working".to_string()];
        let mut page = page(20);

        for high in 0..=CHROME {
            let told = rows(&page.frame(COLS, high, &block, KEYS));
            assert_eq!(told.len(), high.max(1), "{high} rows");
        }
        assert_eq!(rows(&page.frame(4, 3, &block, KEYS)).len(), 3);
    }
}
