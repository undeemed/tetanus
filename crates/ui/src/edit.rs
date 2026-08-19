//! The line a person is typing, and what each keystroke does to it.
//!
//! A terminal left in its ordinary mode already edits a line: it echoes what
//! is typed, honours Backspace, and hands the whole line over at Enter. What
//! it does not do is anything else. An arrow key is not a movement to it, it
//! is three bytes, and they go into the line with the letters. So a prompt
//! that reads a line that way answers `helo` + left + left + `l` with the
//! literal text `helo^[[D^[[Dl`, sends that to the model, and writes it to the
//! journal.
//!
//! This module is the other half: the terminal is held in raw mode, every
//! keystroke arrives as a [`Key`], and this type is what a key means. It holds
//! the text and the cursor, and nothing else - no terminal, no stream, no
//! history - so the whole of it is testable by feeding it keys and reading the
//! text back.
//!
//! # Why the editor draws its own line
//!
//! The caller has to repaint after every keystroke, and repainting means
//! knowing where the cursor sits in columns, which is not where it sits in
//! characters: a CJK character is two columns and a combining mark is none.
//! That arithmetic belongs with the text it is about, so [`Line::repaint`]
//! returns the whole row - marker, text, cursor - and a caller writes it.
//!
//! # Why a long line scrolls rather than wraps
//!
//! A line that wrapped would occupy two rows, and every repaint after it would
//! erase one of them and leave the other. So the row shows a window onto the
//! text, wide enough for what is left beside the marker, and the window
//! follows the cursor. It is the same bargain `Screen` makes for a frame: one
//! line is one row, whatever it holds.

use std::io::{self, Write};
use std::time::Duration;

use crate::terminal::{Key, Keys};
use crate::text::visible_width;

/// How long a read waits before looking again.
///
/// Nothing depends on the length: the wait ends the moment a key arrives, and
/// a wait that passed with none simply starts another. It is bounded at all
/// only so that a reader is not blocked inside a call that cannot be
/// interrupted.
const LOOKING: Duration = Duration::from_secs(1);

/// Read one line, drawing it as it is typed.
///
/// The terminal is the caller's to take and give back - this function reads
/// keys and writes rows, and holds nothing. It returns on the three keystrokes
/// that end a line, having written the newline that closes the row, so what
/// the caller prints next starts on a row of its own.
///
/// A window that changed size arrives on the same queue as the keys, and is
/// answered here rather than passed on: the row is redrawn to the new width,
/// which is the whole of what a line being typed has to do about it.
pub fn read<K: Keys, W: Write>(
    keys: &mut K,
    out: &mut W,
    marker: &str,
    width: usize,
) -> io::Result<Typed> {
    let mut line = Line::new();
    let mut width = width;
    let draw = |line: &mut Line, out: &mut W, width| -> io::Result<()> {
        write!(out, "{}", line.repaint(marker, width))?;
        out.flush()
    };

    draw(&mut line, out, width)?;
    loop {
        let Some(key) = keys.key(LOOKING)? else {
            continue;
        };
        if let Key::Resize(cols, _) = key {
            width = cols as usize;
            draw(&mut line, out, width)?;
            continue;
        }
        match line.key(key) {
            Typed::Editing => draw(&mut line, out, width)?,
            Typed::Ignored => {}
            // Raw mode is the caller's, so the return is written out: the
            // terminal is not translating one into the other while a line is
            // being read this way.
            ended => {
                write!(out, "\r\n")?;
                out.flush()?;
                return Ok(ended);
            }
        }
    }
}

/// What a keystroke did to the line.
///
/// Non-exhaustive because a later slice adds a history to walk with the up and
/// down keys, and a caller that matched exhaustively today would be the thing
/// that stopped compiling for it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Typed {
    /// The line or the cursor moved. Repaint it.
    Editing,
    /// Enter: this is the whole line, and the editor is empty again.
    Asked(String),
    /// Ctrl-D on an empty line, which is how a person says there is no more
    /// input. On a line that holds something it deletes instead, because that
    /// is what every other line editor does with it.
    Left,
    /// Ctrl-C, wherever the cursor was.
    Interrupted,
    /// A key this editor does not answer. Nothing changed, so nothing needs
    /// repainting: a caller that repaints on `Editing` alone is correct.
    Ignored,
}

/// A line being typed, and where the cursor is in it.
///
/// The cursor is an index into the characters, and it may be one past the last
/// of them, which is where it is while a line is being typed onto the end.
#[derive(Debug, Default)]
pub struct Line {
    text: Vec<char>,
    cursor: usize,
    /// First character of the window the row shows. Kept between repaints so
    /// that walking a long line moves the cursor through a still window,
    /// rather than dragging the text under a cursor pinned to one edge.
    offset: usize,
}

impl Line {
    /// An empty line, with the cursor at its start.
    pub fn new() -> Self {
        Self::default()
    }

    /// What has been typed so far.
    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    /// Where the cursor is, counted in characters from the start.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Read one keystroke.
    pub fn key(&mut self, key: Key) -> Typed {
        match key {
            Key::Char(char) => {
                self.text.insert(self.cursor, char);
                self.cursor += 1;
                Typed::Editing
            }
            // Ctrl-J is the same byte a newline is, and a terminal in raw
            // mode is not translating it: it is what the Enter key sends
            // through some terminals, and what every line of a pasted block
            // ends with.
            Key::Enter | Key::Ctrl('j') => {
                let asked = self.text();
                self.text.clear();
                self.cursor = 0;
                self.offset = 0;
                Typed::Asked(asked)
            }
            Key::Ctrl('c') => Typed::Interrupted,
            // The one key whose meaning depends on the line it is pressed on.
            // Empty, it is how a person says there is no more input; on a line
            // that holds something it is a forward delete, and `taken` has it.
            Key::Ctrl('d') if self.text.is_empty() => Typed::Left,
            moved_or_taken => self
                .moved(moved_or_taken)
                .or_else(|| self.taken(moved_or_taken))
                .unwrap_or(Typed::Ignored),
        }
    }

    /// The keys that move the cursor and leave the text alone, answered as
    /// where the cursor goes. `None` for a key that is not one of them.
    ///
    /// Split from [`Line::key`] with [`Line::taken`] because the three answer
    /// different questions - what a key inserts, where it moves, what it takes
    /// out - and one `match` over every key answered all three at once.
    fn moved(&mut self, key: Key) -> Option<Typed> {
        let cursor = match key {
            Key::Left | Key::Ctrl('b') => self.back(1),
            Key::Right | Key::Ctrl('f') => self.forward(1),
            Key::Home | Key::Ctrl('a') => 0,
            Key::End | Key::Ctrl('e') => self.text.len(),
            Key::Alt('b') => self.word_back(),
            Key::Alt('f') => self.word_forward(),
            _ => return None,
        };
        Some(self.move_to(cursor))
    }

    /// The keys that take text out, answered as the range they take. `None`
    /// for a key that is not one of them.
    fn taken(&mut self, key: Key) -> Option<Typed> {
        let (from, to) = match key {
            Key::Backspace | Key::Ctrl('h') => (self.back(1), self.cursor),
            Key::Delete | Key::Ctrl('d') => (self.cursor, self.forward(1)),
            Key::Ctrl('u') => (0, self.cursor),
            Key::Ctrl('k') => (self.cursor, self.text.len()),
            Key::Ctrl('w') => (self.word_back(), self.cursor),
            _ => return None,
        };
        Some(self.erase(from, to))
    }

    /// The row to write for the line as it stands: the marker, the part of the
    /// text the row has room for, and the cursor where it belongs.
    ///
    /// `marker` is written as it was given, painted or not, and measured for
    /// what it draws rather than what it holds. `width` is the terminal's, and
    /// a width with no room left for text still draws the marker: a row that
    /// short is the terminal's answer, not something to arrange around.
    pub fn repaint(&mut self, marker: &str, width: usize) -> String {
        let label = visible_width(marker);
        let room = width.saturating_sub(label);
        let cursor = self.slide(room);
        // A column count of zero is not a move of none: terminals read the
        // parameter `0` as `1`, so the row is left as it is instead.
        let place = match label + cursor {
            0 => String::new(),
            columns => format!("\x1b[{columns}C"),
        };
        format!("\r{marker}\x1b[K{}\r{place}", self.window(room))
    }

    /// As much of the text from the window's start as `room` columns hold.
    ///
    /// Cut and not [`truncate`](crate::truncate)d: the mark that function adds
    /// says a value was too long to show, and this text is not too long, it is
    /// scrolled. A mark would also take a column from the line being typed.
    fn window(&self, room: usize) -> String {
        let mut columns = 0;
        let mut shown = String::new();
        for &char in &self.text[self.offset..] {
            columns += visible_width(&char.to_string());
            if columns > room {
                break;
            }
            shown.push(char);
        }
        shown
    }

    /// Move the window so the cursor is inside it, and answer where the cursor
    /// then sits, in columns from the start of the text's part of the row.
    fn slide(&mut self, room: usize) -> usize {
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        // One column is kept free at the right edge for the cursor itself,
        // which is why the window ends before the character it is on.
        let last = room.saturating_sub(1);
        while self.columns(self.offset, self.cursor) > last {
            self.offset += 1;
        }
        self.columns(self.offset, self.cursor)
    }

    /// The columns the characters in `from..to` draw in.
    fn columns(&self, from: usize, to: usize) -> usize {
        visible_width(&self.text[from..to].iter().collect::<String>())
    }

    /// Take out `from..to`, and leave the cursor where the text closed up.
    fn erase(&mut self, from: usize, to: usize) -> Typed {
        if from >= to {
            return Typed::Ignored;
        }
        self.text.drain(from..to);
        self.cursor = from;
        Typed::Editing
    }

    fn move_to(&mut self, cursor: usize) -> Typed {
        if cursor == self.cursor {
            return Typed::Ignored;
        }
        self.cursor = cursor;
        Typed::Editing
    }

    fn back(&self, by: usize) -> usize {
        self.cursor.saturating_sub(by)
    }

    fn forward(&self, by: usize) -> usize {
        (self.cursor + by).min(self.text.len())
    }

    /// The start of the word the cursor is in or just after.
    ///
    /// The spaces before the cursor go with it, so Ctrl-W on `two words   `
    /// leaves `two ` rather than `two words` with the cursor in a gap.
    fn word_back(&self) -> usize {
        let mut at = self.cursor;
        while at > 0 && self.text[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !self.text[at - 1].is_whitespace() {
            at -= 1;
        }
        at
    }

    /// The end of the word the cursor is in or just before.
    fn word_forward(&self) -> usize {
        let mut at = self.cursor;
        while at < self.text.len() && self.text[at].is_whitespace() {
            at += 1;
        }
        while at < self.text.len() && !self.text[at].is_whitespace() {
            at += 1;
        }
        at
    }
}

/// Test Design Specification: what a keystroke does to the line being typed.
///
/// Features tested: that a character lands where the cursor is and not at the
/// end, which is the whole reason this type exists; that Enter hands the line
/// over and leaves an empty one; that every movement, deletion and kill key
/// this editor answers does what a shell's does, including at both ends of the
/// line, where several of them do nothing; that Ctrl-D means two different
/// things on an empty line and on a full one; that a key outside the map
/// changes nothing and says so; that the row a repaint writes is the marker,
/// the text and the cursor, in columns rather than characters; and that a line
/// longer than the row scrolls under a cursor that stays where the reader put
/// it.
///
/// It also covers the read loop over those keystrokes: that it draws the row
/// once before anything is typed and once per change, that a wait which passed
/// with nothing is not a keystroke, that a window which changed size is
/// answered by redrawing at the new width, and that each of the three keys
/// that end a line writes the return that closes the row.
///
/// Features NOT tested here: the terminal itself - taking raw mode and
/// supplying real keystrokes is [`Typing`](crate::Typing), which needs a
/// controlling terminal a `cargo test` process does not have, and which
/// `target/probe-edit.py` drives on a pty instead. Nor what a finished line
/// means: `tetanus chat` decides what is a question and what is a command.
///
/// Environmental needs: none. Every case feeds keys to a value and reads it
/// back; no case opens a terminal.
#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal that answers the keys it was given, and nothing after them.
    /// A `None` in the script is a wait that passed with nothing typed, which
    /// is what a real one returns while a person is thinking.
    struct Scripted(std::vec::IntoIter<Option<Key>>);

    impl Keys for Scripted {
        fn key(&mut self, _wait: Duration) -> io::Result<Option<Key>> {
            match self.0.next() {
                Some(key) => Ok(key),
                // The script ran out without a key that ends the line, which
                // is a case being written wrong rather than anything a
                // terminal does.
                None => Err(io::Error::other("the script ran out")),
            }
        }
    }

    /// Read one line from a script, and answer it with what was drawn.
    fn read_from(script: Vec<Option<Key>>, width: usize) -> (Typed, String) {
        let mut keys = Scripted(script.into_iter());
        let mut out = Vec::new();
        let read = read(&mut keys, &mut out, "> ", width).expect("the script reads");
        (read, String::from_utf8(out).expect("utf-8"))
    }

    /// Type `text` into a fresh line, character by character.
    fn typed(text: &str) -> Line {
        let mut line = Line::new();
        for char in text.chars() {
            line.key(Key::Char(char));
        }
        line
    }

    /// Feed keys and answer the text and cursor they left behind.
    fn after(text: &str, keys: &[Key]) -> (String, usize) {
        let mut line = typed(text);
        for key in keys {
            line.key(*key);
        }
        (line.text(), line.cursor())
    }

    /// TC-UI-EDIT-1: a character typed where the cursor is.
    /// Expected: `hello`. This is the case the module exists for: a terminal
    /// in its ordinary mode answers the same keystrokes with the literal text
    /// `helo^[[D^[[Dl`, and sends that to the model.
    #[test]
    fn a_character_lands_at_the_cursor_and_not_at_the_end() {
        assert_eq!(
            after("helo", &[Key::Left, Key::Left, Key::Char('l')]),
            ("hello".into(), 3)
        );
    }

    /// TC-UI-EDIT-2: Enter.
    /// Expected: the whole line, once, and an empty editor after it - a second
    /// Enter answers with the empty line rather than the one before it. What
    /// an empty line means is the caller's to decide, so it is handed over
    /// like any other. Ctrl-J does the same: it is the byte a newline is, and
    /// a terminal in raw mode hands it over untranslated.
    #[test]
    fn enter_hands_the_line_over_and_keeps_none_of_it() {
        let mut line = typed("ask me");
        assert_eq!(line.key(Key::Enter), Typed::Asked("ask me".into()));
        assert_eq!(line.text(), "");
        assert_eq!(line.cursor(), 0);
        assert_eq!(line.key(Key::Enter), Typed::Asked(String::new()));

        let mut line = typed("ask me");
        assert_eq!(line.key(Key::Ctrl('j')), Typed::Asked("ask me".into()));
    }

    /// TC-UI-EDIT-3: the movement keys, including at both ends.
    /// Expected: the cursor where a shell would leave it, and `Ignored` from
    /// the ones that had nowhere to go. A caller repaints on `Editing`, so a
    /// key that moved nothing must not claim it did.
    #[test]
    fn the_cursor_moves_the_way_a_shell_moves_it() {
        let cases: [(&[Key], usize); 8] = [
            (&[Key::Home], 0),
            (&[Key::Ctrl('a')], 0),
            (&[Key::End], 9),
            (&[Key::Home, Key::Ctrl('e')], 9),
            (&[Key::Home, Key::Right, Key::Ctrl('f')], 2),
            (&[Key::Left, Key::Ctrl('b')], 7),
            (&[Key::Alt('b')], 4),
            (&[Key::Home, Key::Alt('f')], 3),
        ];
        for (keys, cursor) in cases {
            assert_eq!(
                after("two words", keys),
                ("two words".into(), cursor),
                "{keys:?}"
            );
        }

        let mut line = typed("two words");
        assert_eq!(line.key(Key::End), Typed::Ignored, "already at the end");
        assert_eq!(line.key(Key::Right), Typed::Ignored, "nowhere to the right");
        line.key(Key::Home);
        assert_eq!(line.key(Key::Left), Typed::Ignored, "nowhere to the left");
    }

    /// TC-UI-EDIT-4: the deletion and kill keys.
    /// Expected: one character for Backspace and Delete, the line before or
    /// after the cursor for Ctrl-U and Ctrl-K, and the word before it for
    /// Ctrl-W - with the spaces in front of the cursor going with the word,
    /// which is what leaves `two ` and not a gap.
    #[test]
    fn the_deletion_keys_take_what_a_shell_takes() {
        let cases: [(&str, &[Key], &str, usize); 7] = [
            ("word", &[Key::Backspace], "wor", 3),
            ("word", &[Key::Ctrl('h')], "wor", 3),
            ("word", &[Key::Home, Key::Delete], "ord", 0),
            ("two words", &[Key::Alt('b'), Key::Ctrl('u')], "words", 0),
            ("two words", &[Key::Alt('b'), Key::Ctrl('k')], "two ", 4),
            ("two words   ", &[Key::Ctrl('w')], "two ", 4),
            ("two words", &[Key::Home, Key::Ctrl('w')], "two words", 0),
        ];
        for (text, keys, left, cursor) in cases {
            assert_eq!(after(text, keys), (left.into(), cursor), "{keys:?}");
        }

        let mut line = typed("word");
        assert_eq!(line.key(Key::Delete), Typed::Ignored, "nothing after it");
        line.key(Key::Home);
        assert_eq!(
            line.key(Key::Backspace),
            Typed::Ignored,
            "nothing before it"
        );
    }

    /// TC-UI-EDIT-5: Ctrl-D, on an empty line and on a full one.
    /// Expected: `Left` only when there is nothing to delete. It is the one
    /// key whose meaning depends on the line it is pressed on, and a chat that
    /// read it as "leave" while a half-typed question was on the screen would
    /// throw the question away.
    #[test]
    fn control_d_leaves_only_an_empty_line() {
        assert_eq!(Line::new().key(Key::Ctrl('d')), Typed::Left);

        let mut line = typed("word");
        line.key(Key::Home);
        assert_eq!(line.key(Key::Ctrl('d')), Typed::Editing);
        assert_eq!(line.text(), "ord");
        line.key(Key::End);
        assert_eq!(
            line.key(Key::Ctrl('d')),
            Typed::Ignored,
            "nothing to delete"
        );
        assert_eq!(line.text(), "ord");
    }

    /// TC-UI-EDIT-6: Ctrl-C.
    /// Expected: `Interrupted`, wherever the cursor is, and the line left as
    /// it was. The caller decides what to do with what was typed, and one of
    /// the things it may do is print it.
    #[test]
    fn control_c_interrupts_and_keeps_the_line() {
        let mut line = typed("half a question");
        line.key(Key::Home);
        assert_eq!(line.key(Key::Ctrl('c')), Typed::Interrupted);
        assert_eq!(line.text(), "half a question");
    }

    /// TC-UI-EDIT-7: every key the vocabulary names and this editor does not
    /// answer.
    /// Expected: `Ignored`, and a line that did not move. A key that fell
    /// through to a default and inserted itself would put `[A` in a question.
    #[test]
    fn a_key_outside_the_map_changes_nothing() {
        for key in [
            Key::Tab,
            Key::BackTab,
            Key::Up,
            Key::Down,
            Key::PageUp,
            Key::PageDown,
            Key::Esc,
            Key::Alt('x'),
            Key::Ctrl('z'),
            Key::Resize(80, 24),
        ] {
            let mut line = typed("steady");
            assert_eq!(line.key(key), Typed::Ignored, "{key:?}");
            assert_eq!(
                (line.text(), line.cursor()),
                ("steady".into(), 6),
                "{key:?}"
            );
        }
    }

    /// TC-UI-EDIT-8: the row a repaint writes.
    /// Expected: return to column zero, the marker as it was given, erase what
    /// the last row left, the text, and the cursor placed in columns from the
    /// left edge. The marker is measured for what it draws, so a painted one
    /// places the cursor in the same column as a plain one.
    #[test]
    fn a_repaint_is_the_marker_the_text_and_the_cursor() {
        let mut line = typed("hi");
        assert_eq!(line.repaint("> ", 20), "\r> \x1b[Khi\r\x1b[4C");
        assert_eq!(
            typed("hi").repaint("\x1b[36m›\x1b[0m ", 20),
            "\r\x1b[36m›\x1b[0m \x1b[Khi\r\x1b[4C"
        );

        // Nothing typed yet, and no marker: the return is the whole of the
        // placement, because `\x1b[0C` is not a move of none - a terminal
        // reads the parameter `0` as `1` and would put the cursor one column
        // to the right of where the reader types.
        assert_eq!(Line::new().repaint("", 20), "\r\x1b[K\r");
    }

    /// TC-UI-EDIT-9: a character that draws in two columns.
    /// Expected: the cursor placed by columns and not by characters. Placing
    /// it by characters would leave it inside the last character on every row
    /// holding CJK text.
    #[test]
    fn the_cursor_is_placed_in_columns() {
        assert_eq!(typed("漢字").repaint("> ", 20), "\r> \x1b[K漢字\r\x1b[6C");
    }

    /// TC-UI-EDIT-10: a line longer than the row.
    /// Expected: one row, always, showing the end of the line while it is
    /// being typed, and the start of it again when the cursor goes back there.
    /// A row that wrapped would leave half of itself behind at the next
    /// repaint.
    #[test]
    fn a_long_line_scrolls_under_the_row() {
        let mut line = typed("abcdefghij");
        assert_eq!(line.repaint("> ", 10), "\r> \x1b[Kdefghij\r\x1b[9C");

        line.key(Key::Home);
        assert_eq!(line.repaint("> ", 10), "\r> \x1b[Kabcdefgh\r\x1b[2C");

        // The window stays where it is while the cursor moves inside it, and
        // follows only when the cursor reaches its edge.
        line.key(Key::End);
        line.key(Key::Left);
        assert_eq!(line.repaint("> ", 10), "\r> \x1b[Kcdefghij\r\x1b[9C");

        // A row with no room for text still draws the marker.
        assert_eq!(typed("abc").repaint("> ", 2), "\r> \x1b[K\r\x1b[2C");
    }

    /// TC-UI-EDIT-11: text held as characters, not bytes.
    /// Expected: one Backspace takes one character, whatever it is made of. A
    /// byte-indexed editor cuts a multi-byte character in half and writes the
    /// halves to the journal.
    #[test]
    fn one_backspace_takes_one_character() {
        assert_eq!(after("café", &[Key::Backspace]), ("caf".into(), 3));
        assert_eq!(after("hi 🙂", &[Key::Backspace]), ("hi ".into(), 3));
    }

    /// TC-UI-EDIT-12: a line typed and entered.
    /// Expected: the line, and a row drawn once before anything was typed and
    /// once for each key that changed it, closing with the return that ends
    /// it. The first draw is what puts the marker on the screen, so a prompt
    /// that printed nothing until the first keystroke would look like a hung
    /// process.
    #[test]
    fn a_read_draws_the_row_and_answers_with_the_line() {
        let (read, drawn) = read_from(
            vec![Some(Key::Char('h')), Some(Key::Char('i')), Some(Key::Enter)],
            20,
        );

        assert_eq!(read, Typed::Asked("hi".into()));
        assert_eq!(
            drawn,
            "\r> \x1b[K\r\x1b[2C\
             \r> \x1b[Kh\r\x1b[3C\
             \r> \x1b[Khi\r\x1b[4C\
             \r\n"
        );
    }

    /// TC-UI-EDIT-13: a wait that passed with nothing typed, and a key that
    /// changed nothing.
    /// Expected: no row for either. A prompt that redrew itself once a second
    /// while nobody typed would fight anything else writing to the terminal.
    #[test]
    fn nothing_typed_draws_nothing() {
        let (read, drawn) = read_from(vec![None, None, Some(Key::Left), Some(Key::Enter)], 20);

        assert_eq!(read, Typed::Asked(String::new()));
        assert_eq!(drawn, "\r> \x1b[K\r\x1b[2C\r\n");
    }

    /// TC-UI-EDIT-14: the window changed size while a line was being typed.
    /// Expected: the row redrawn to the new width, which for a line longer
    /// than the new one means the window over it moves. The size arrives on
    /// the same queue as the keys because it has to be answered in the same
    /// place: the row on the screen was drawn for a width that no longer
    /// exists.
    #[test]
    fn a_resize_redraws_the_row_at_the_new_width() {
        let typing = "abcdefghij".chars().map(|char| Some(Key::Char(char)));
        let (read, drawn) = read_from(
            typing
                .chain([Some(Key::Resize(8, 24)), Some(Key::Enter)])
                .collect(),
            20,
        );

        assert_eq!(read, Typed::Asked("abcdefghij".into()));
        // Two returns to a row - one to draw it, one to place the cursor -
        // so the row before the resize is five pieces from the end and the
        // row after it is three.
        let rows: Vec<&str> = drawn.split('\r').collect();
        assert_eq!(rows[rows.len() - 5], "> \x1b[Kabcdefghij");
        assert_eq!(rows[rows.len() - 3], "> \x1b[Kfghij", "the window moved");
    }

    /// TC-UI-EDIT-15: the other two keys that end a line.
    /// Expected: each returns what it means and closes the row, so that
    /// whatever the caller prints about it starts on a row of its own rather
    /// than beside the marker.
    #[test]
    fn every_way_out_closes_the_row() {
        for (key, expected) in [
            (Key::Ctrl('c'), Typed::Interrupted),
            (Key::Ctrl('d'), Typed::Left),
        ] {
            let (read, drawn) = read_from(vec![Some(key)], 20);
            assert_eq!(read, expected);
            assert!(drawn.ends_with("\r\n"), "{drawn:?}");
        }
    }
}
