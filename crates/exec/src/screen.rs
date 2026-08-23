//! What a terminal would be *showing*, as against what was printed on it.
//!
//! [`crate::sanitize`] answers the first question a reader has - what did this
//! program say - by throwing the control language away and keeping the text.
//! For `ls`, `cargo build` and every other program that prints and moves on,
//! that is the whole answer.
//!
//! It is the wrong answer for a program that *draws*. `htop` prints its
//! header once and then spends the rest of the session moving the cursor and
//! overwriting cells; `vim` paints a screen, switches to an alternate buffer
//! and repaints on every keystroke. Sanitized, both produce a transcript that
//! is every frame concatenated: thousands of lines, none of them what is on
//! the screen, and no way for a reader to tell which parts are current. This
//! is why `docs/parity.md` said such programs were *runnable here and not
//! readable*.
//!
//! So this keeps a screen: a grid of cells, a cursor, a scrolling region, and
//! the sequences that move them. It is fed the same bytes the sanitizer is fed
//! and answers the other question - what would somebody looking at this
//! terminal see right now.
//!
//! **What it implements, and what it deliberately does not.** The cursor and
//! erase family (`CUP`, `CUU`/`CUD`/`CUF`/`CUB`, `ED`, `EL`), line and
//! character insertion and deletion, the scrolling region, autowrap, the saved
//! cursor, and the alternate screen buffer - which is the one that matters
//! most, because entering it is a program announcing that it is about to draw
//! rather than print. It does *not* keep colours or attributes: a model reads
//! text, and an attribute model would double the size of this file to record
//! something nothing here can render. It is not a terminal emulator anybody
//! should point a real user at; it is enough to answer "what is on the screen"
//! for the programs that make that question meaningful.
//!
//! **Both models are kept, and neither replaces the other.** A program that
//! prints wants the transcript; a program that draws wants the screen; a
//! session does both at different times. Keeping the screen costs
//! `rows * cols` cells - 40 x 160 here, a few kilobytes - which is what makes
//! it affordable to keep always rather than only when something asks.

use std::sync::Mutex;

/// One terminal screen: the grid, the cursor, and the state a program's
/// escape sequences change.
#[derive(Debug)]
pub struct Screen {
    state: Mutex<Grid>,
}

/// The cursor's place, counted from the top-left as a person counts rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug)]
struct Grid {
    rows: usize,
    cols: usize,
    /// The visible cells, row-major.
    cells: Vec<Vec<char>>,
    cursor: Cursor,
    /// A cursor saved by `ESC 7` or `CSI s`, restored by its partner.
    saved: Cursor,
    /// The scrolling region, as rows, inclusive. A program that sets one is
    /// telling the terminal which part of the screen scrolls and which part
    /// stays - a status line at the bottom, usually.
    top: usize,
    bottom: usize,
    /// Whether writing past the last column moves to the next line.
    autowrap: bool,
    /// The grid the program left when it switched to the alternate screen, so
    /// leaving gives it back. `vim` and `htop` both do this, which is why a
    /// shell's scrollback is still there when they exit.
    stashed: Option<Box<Grid>>,
    /// Whether this is the alternate screen. It is the most useful single bit
    /// in this file: a program that entered it is drawing rather than
    /// printing, and a reader wants the grid rather than the transcript.
    alternate: bool,
    /// A sequence that has begun and not finished, carried between feeds.
    pending: String,
}

impl Screen {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            state: Mutex::new(Grid::new(rows.max(1) as usize, cols.max(1) as usize)),
        }
    }

    /// Feed the terminal's raw output, escapes and all.
    pub fn feed(&self, bytes: &str) {
        self.state
            .lock()
            .expect("no panic holds this lock")
            .feed(bytes);
    }

    /// What is on the screen now, one line per row, with trailing blanks
    /// removed.
    ///
    /// Trailing blank *rows* go too: a program drawing five lines on a
    /// forty-row terminal means five lines, and thirty-five empty ones would
    /// be a reader's whole page.
    pub fn text(&self) -> String {
        self.state.lock().expect("no panic holds this lock").text()
    }

    /// Where the cursor is, which is what a reader needs to know which field a
    /// form is asking about.
    pub fn cursor(&self) -> Cursor {
        self.state.lock().expect("no panic holds this lock").cursor
    }

    /// Whether the program on this terminal has switched to the alternate
    /// screen - which is a program saying, in the only way a terminal has,
    /// that it is drawing rather than printing.
    pub fn is_alternate(&self) -> bool {
        self.state
            .lock()
            .expect("no panic holds this lock")
            .alternate
    }

    /// Change the screen's shape, keeping what fits.
    pub fn resize(&self, rows: u16, cols: u16) {
        self.state
            .lock()
            .expect("no panic holds this lock")
            .resize(rows.max(1) as usize, cols.max(1) as usize);
    }
}

impl Grid {
    fn new(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            cells: vec![vec![' '; cols]; rows],
            cursor: Cursor { row: 0, col: 0 },
            saved: Cursor { row: 0, col: 0 },
            top: 0,
            bottom: rows - 1,
            autowrap: true,
            stashed: None,
            alternate: false,
            pending: String::new(),
        }
    }

    fn text(&self) -> String {
        let mut lines: Vec<String> = self
            .cells
            .iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect();
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    fn feed(&mut self, bytes: &str) {
        let carried = std::mem::take(&mut self.pending);
        let text = format!("{carried}{bytes}");
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let Some(at) = rest.find('\u{1b}') else {
                self.print(rest);
                return;
            };
            let (before, from_escape) = rest.split_at(at);
            self.print(before);
            match self.escape(from_escape) {
                // Consumed `used` bytes of a complete sequence.
                Some(used) => rest = &from_escape[used..],
                // Incomplete: carry it and wait for the rest of the stream.
                None => {
                    self.pending = from_escape.to_string();
                    return;
                }
            }
        }
    }

    /// Handle one escape sequence at the start of `text`, answering how many
    /// bytes it took, or `None` if it has not finished arriving.
    fn escape(&mut self, text: &str) -> Option<usize> {
        let mut chars = text.char_indices().skip(1);
        let (_, kind) = chars.next()?;
        match kind {
            '[' => self.csi(text),
            ']' => Self::osc(text),
            // `ESC 7` / `ESC 8`: the cursor save and restore every full-screen
            // program uses before and after drawing somewhere else.
            '7' => {
                self.saved = self.cursor;
                Some(2)
            }
            '8' => {
                self.cursor = self.saved;
                Some(2)
            }
            // `ESC M`: reverse index, which is how a program scrolls down at
            // the top of a region.
            'M' => {
                self.reverse_index();
                Some(2)
            }
            // Charset selection and the rest of the two-byte family: nothing
            // here renders glyphs differently, so they are skipped whole.
            _ => Some(1 + kind.len_utf8()),
        }
    }

    /// A CSI sequence: `ESC [ params final`.
    fn csi(&mut self, text: &str) -> Option<usize> {
        let body = &text[2..];
        let end = body.find(|c: char| ('\u{40}'..='\u{7e}').contains(&c))?;
        let (params, final_byte) = body.split_at(end);
        let final_byte = final_byte.chars().next()?;
        let private = params.starts_with('?');
        let numbers: Vec<usize> = params
            .trim_start_matches('?')
            .split(';')
            .map(|value| value.trim().parse::<usize>().unwrap_or(0))
            .collect();
        let first = numbers.first().copied().unwrap_or(0);
        let count = first.max(1);

        match (private, final_byte) {
            // The alternate screen. `?1049` saves the cursor and switches;
            // `?47` is the older spelling of the same idea.
            (true, 'h') if matches!(first, 1049 | 47 | 1047) => self.enter_alternate(),
            (true, 'l') if matches!(first, 1049 | 47 | 1047) => self.leave_alternate(),
            (true, 'h') if first == 7 => self.autowrap = true,
            (true, 'l') if first == 7 => self.autowrap = false,
            // Everything else private - bracketed paste, cursor visibility,
            // mouse reporting - changes nothing this model keeps.
            (true, _) => {}
            (false, 'H' | 'f') => {
                let row = numbers.first().copied().unwrap_or(1).max(1) - 1;
                let col = numbers.get(1).copied().unwrap_or(1).max(1) - 1;
                self.cursor = Cursor {
                    row: row.min(self.rows - 1),
                    col: col.min(self.cols - 1),
                };
            }
            (false, 'A') => self.cursor.row = self.cursor.row.saturating_sub(count),
            (false, 'B') => self.cursor.row = (self.cursor.row + count).min(self.rows - 1),
            (false, 'C') => self.cursor.col = (self.cursor.col + count).min(self.cols - 1),
            (false, 'D') => self.cursor.col = self.cursor.col.saturating_sub(count),
            (false, 'G') => self.cursor.col = (count - 1).min(self.cols - 1),
            (false, 'd') => self.cursor.row = (count - 1).min(self.rows - 1),
            (false, 'E') => {
                self.cursor.row = (self.cursor.row + count).min(self.rows - 1);
                self.cursor.col = 0;
            }
            (false, 'F') => {
                self.cursor.row = self.cursor.row.saturating_sub(count);
                self.cursor.col = 0;
            }
            (false, 'J') => self.erase_display(first),
            (false, 'K') => self.erase_line(first),
            (false, 'L') => self.insert_lines(count),
            (false, 'M') => self.delete_lines(count),
            (false, '@') => self.insert_cells(count),
            (false, 'P') => self.delete_cells(count),
            (false, 'X') => self.erase_cells(count),
            (false, 'r') => {
                let top = numbers.first().copied().unwrap_or(1).max(1) - 1;
                let bottom = numbers
                    .get(1)
                    .copied()
                    .filter(|value| *value > 0)
                    .unwrap_or(self.rows)
                    - 1;
                if top < bottom && bottom < self.rows {
                    self.top = top;
                    self.bottom = bottom;
                }
                self.cursor = Cursor { row: 0, col: 0 };
            }
            (false, 's') => self.saved = self.cursor,
            (false, 'u') => self.cursor = self.saved,
            // `m` is colour and attributes, which this model does not keep;
            // the rest are reports and modes nothing here answers.
            (false, _) => {}
        }
        Some(2 + end + final_byte.len_utf8())
    }

    /// An OSC sequence, which sets a title or talks to the host. Nothing here
    /// draws, so the whole thing is skipped - including the prompt marker,
    /// which `crate::sanitize` is the one that reads.
    fn osc(text: &str) -> Option<usize> {
        let body = &text[2..];
        if let Some(at) = body.find('\u{7}') {
            return Some(2 + at + 1);
        }
        body.find("\u{1b}\\").map(|at| 2 + at + 2)
    }

    fn print(&mut self, text: &str) {
        for glyph in text.chars() {
            match glyph {
                '\r' => self.cursor.col = 0,
                '\n' => self.line_feed(),
                '\t' => self.cursor.col = ((self.cursor.col / 8) + 1) * 8,
                '\u{8}' => self.cursor.col = self.cursor.col.saturating_sub(1),
                '\u{7}' => {}
                glyph if (glyph as u32) < 0x20 => {}
                glyph => {
                    if self.cursor.col >= self.cols {
                        if !self.autowrap {
                            self.cursor.col = self.cols - 1;
                        } else {
                            self.cursor.col = 0;
                            self.line_feed();
                        }
                    }
                    let row = self.cursor.row.min(self.rows - 1);
                    let col = self.cursor.col.min(self.cols - 1);
                    self.cells[row][col] = glyph;
                    self.cursor.col += 1;
                }
            }
            if self.cursor.col > self.cols {
                self.cursor.col = self.cols;
            }
        }
    }

    fn line_feed(&mut self) {
        if self.cursor.row == self.bottom {
            self.scroll_up(1);
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cursor.row == self.top {
            self.scroll_down(1);
        } else {
            self.cursor.row = self.cursor.row.saturating_sub(1);
        }
    }

    fn scroll_up(&mut self, lines: usize) {
        for _ in 0..lines {
            self.cells.remove(self.top);
            self.cells.insert(self.bottom, vec![' '; self.cols]);
        }
    }

    fn scroll_down(&mut self, lines: usize) {
        for _ in 0..lines {
            self.cells.remove(self.bottom);
            self.cells.insert(self.top, vec![' '; self.cols]);
        }
    }

    fn erase_display(&mut self, how: usize) {
        match how {
            // To the end of the screen.
            0 => {
                self.erase_line(0);
                for row in self.cursor.row + 1..self.rows {
                    self.cells[row] = vec![' '; self.cols];
                }
            }
            // From the beginning.
            1 => {
                self.erase_line(1);
                for row in 0..self.cursor.row {
                    self.cells[row] = vec![' '; self.cols];
                }
            }
            // The whole screen, which is what a program clears with before it
            // draws its first frame.
            _ => {
                for row in 0..self.rows {
                    self.cells[row] = vec![' '; self.cols];
                }
            }
        }
    }

    fn erase_line(&mut self, how: usize) {
        let row = self.cursor.row.min(self.rows - 1);
        let col = self.cursor.col.min(self.cols);
        match how {
            0 => {
                for cell in col..self.cols {
                    self.cells[row][cell] = ' ';
                }
            }
            1 => {
                for cell in 0..=col.min(self.cols - 1) {
                    self.cells[row][cell] = ' ';
                }
            }
            _ => self.cells[row] = vec![' '; self.cols],
        }
    }

    fn insert_lines(&mut self, lines: usize) {
        if self.cursor.row < self.top || self.cursor.row > self.bottom {
            return;
        }
        for _ in 0..lines {
            self.cells.remove(self.bottom);
            self.cells.insert(self.cursor.row, vec![' '; self.cols]);
        }
    }

    fn delete_lines(&mut self, lines: usize) {
        if self.cursor.row < self.top || self.cursor.row > self.bottom {
            return;
        }
        for _ in 0..lines {
            self.cells.remove(self.cursor.row);
            self.cells.insert(self.bottom, vec![' '; self.cols]);
        }
    }

    fn insert_cells(&mut self, cells: usize) {
        let row = self.cursor.row.min(self.rows - 1);
        for _ in 0..cells {
            self.cells[row].pop();
            self.cells[row].insert(self.cursor.col.min(self.cols - 1), ' ');
        }
    }

    fn delete_cells(&mut self, cells: usize) {
        let row = self.cursor.row.min(self.rows - 1);
        for _ in 0..cells {
            if self.cursor.col < self.cols {
                self.cells[row].remove(self.cursor.col);
                self.cells[row].push(' ');
            }
        }
    }

    fn erase_cells(&mut self, cells: usize) {
        let row = self.cursor.row.min(self.rows - 1);
        for cell in self.cursor.col..(self.cursor.col + cells).min(self.cols) {
            self.cells[row][cell] = ' ';
        }
    }

    fn enter_alternate(&mut self) {
        if self.alternate {
            return;
        }
        let mut stashed = Grid::new(self.rows, self.cols);
        std::mem::swap(&mut stashed, self);
        self.stashed = Some(Box::new(stashed));
        self.alternate = true;
    }

    fn leave_alternate(&mut self) {
        let Some(stashed) = self.stashed.take() else {
            return;
        };
        let pending = std::mem::take(&mut self.pending);
        *self = *stashed;
        self.pending = pending;
        self.alternate = false;
    }

    fn resize(&mut self, rows: usize, cols: usize) {
        self.cells.resize(rows, vec![' '; cols]);
        for row in &mut self.cells {
            row.resize(cols, ' ');
        }
        self.rows = rows;
        self.cols = cols;
        self.top = 0;
        self.bottom = rows - 1;
        self.cursor = Cursor {
            row: self.cursor.row.min(rows - 1),
            col: self.cursor.col.min(cols - 1),
        };
        if let Some(stashed) = self.stashed.as_mut() {
            stashed.resize(rows, cols);
        }
    }
}
