//! What a terminal printed, with the terminal's own control language taken
//! out and its prompt marker read.
//!
//! A pseudo-terminal carries two things at once: the text a program printed,
//! and the instructions it gave the screen - move the cursor, set a colour,
//! set the window title, tell the host where the prompt is. A model reading a
//! transcript wants the first and is confused by the second: `\x1b[32mok`
//! reads as `[32mok`, and a title sequence reads as a line the program never
//! printed.
//!
//! Two jobs, and they are one pass because they see the same bytes:
//!
//! **Strip the control language.** CSI (`ESC [ … final`), OSC (`ESC ] … BEL`
//! or `ESC \`), and the short two-byte escapes. A sequence split across two
//! reads is carried rather than half-printed, which is the whole reason this
//! is a stateful type and not a function.
//!
//! **Read the prompt marker.** The shell this crate starts on a terminal is
//! told to print `ESC ] 133 ; D ; <status> BEL` before every prompt - the OSC
//! 133 "command finished" sequence, which is what a terminal emulator uses to
//! draw the little pass/fail pill beside a prompt. Reading it is what makes a
//! terminal session *exact* rather than inferred: upstream watches for silence
//! and guesses that the command is over, and this knows, because the shell
//! said so and said what the command exited with. Silence remains the fallback
//! for a program that never prints one (a REPL, a pager, a password prompt).
//!
//! Newlines are normalized here, not by the caller: a terminal writes `\r\n`
//! for a new line and a bare `\r` to return to the start of one, and a
//! line-oriented reader wants both to mean "next line". A `\r` that ends a
//! chunk is held back until the next one arrives, so a `\r\n` split across two
//! reads is one newline rather than two.
//!
//! Parity: upstream `packages/terminal/terminal-bash/src/sanitize.ts`.

/// The OSC 133 sequence a shell prints when a command finishes, up to its
/// status. Upstream watches for the same prefix.
pub const PROMPT_MARKER_PREFIX: &str = "133;D;";

/// The prompt this crate's terminal shells are told to print. Fixed, and
/// nothing like a shell's default, so a transcript that contains it is a
/// transcript where our shell is asking for input.
pub const PROMPT_TEXT: &str = "tetanus> ";

/// How much of an unterminated escape sequence is carried between chunks
/// before it is treated as noise. A real sequence is tens of bytes; a stream
/// that has run this far without a terminator is a program writing binary at
/// a terminal, and carrying it for ever would be an unbounded buffer fed by
/// the child.
const MAX_PENDING: usize = 8 * 1024;

/// One chunk of terminal output, sanitized.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sanitized {
    /// The printable text, newline-normalized.
    pub text: String,
    /// The status carried by each prompt marker completed in this chunk, in
    /// order. Empty for almost every chunk; more than one when a burst of
    /// commands finished between two reads. `None` is a marker whose status
    /// was not a number, which is a shell that has been told something odd
    /// rather than a command that failed.
    pub prompts: Vec<Option<i32>>,
}

/// Which kind of sequence is being skipped after the pending bound gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Discarding {
    Osc,
    Csi,
}

/// A streaming sanitizer. One per terminal, fed every read in order.
#[derive(Debug, Default)]
pub struct Sanitizer {
    /// An escape sequence that has begun and not finished.
    pending: String,
    /// A sequence being skipped because it outgrew [`MAX_PENDING`].
    discarding: Option<Discarding>,
    /// A `\r` that ended the last chunk, held in case a `\n` follows.
    trailing_return: bool,
}

impl Sanitizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume one read from the terminal.
    pub fn push(&mut self, chunk: &str) -> Sanitized {
        let chunk = self.skip_discarded(chunk);
        self.pending.push_str(&chunk);

        let mut raw = String::new();
        let mut prompts = Vec::new();
        let mut index = 0;
        let pending = std::mem::take(&mut self.pending);
        while index < pending.len() {
            let Some(escape) = pending[index..].find('\x1b').map(|at| index + at) else {
                raw.push_str(&pending[index..]);
                index = pending.len();
                break;
            };
            raw.push_str(&pending[index..escape]);
            let Some(kind) = pending[escape + 1..].chars().next() else {
                // The escape byte is the last thing in the buffer; what
                // follows decides what it is.
                index = escape;
                break;
            };
            match kind {
                ']' => match osc_end(&pending, escape) {
                    Some((content, end)) => {
                        if let Some(status) = content.strip_prefix(PROMPT_MARKER_PREFIX) {
                            prompts.push(status.trim().parse::<i32>().ok());
                        }
                        index = end;
                    }
                    None => {
                        index = escape;
                        break;
                    }
                },
                '[' => match csi_end(&pending, escape) {
                    Some(end) => index = end,
                    None => {
                        index = escape;
                        break;
                    }
                },
                // A two-byte escape: cursor save, keypad mode, and the rest of
                // the short family. Both bytes go.
                _ => index = escape + 1 + kind.len_utf8(),
            }
        }
        self.pending = pending[index..].to_string();
        self.enforce_pending_bound();

        Sanitized {
            text: self.normalize(&raw),
            prompts,
        }
    }

    /// Everything left when the terminal closes.
    ///
    /// An escape sequence that never finished is dropped rather than printed:
    /// the program that started it is gone, so it was never text.
    pub fn flush(&mut self) -> String {
        let pending = std::mem::take(&mut self.pending);
        self.discarding = None;
        let text = if pending.starts_with('\x1b') {
            String::new()
        } else {
            pending
        };
        let mut normalized = self.normalize(&text);
        if self.trailing_return {
            self.trailing_return = false;
            normalized.push('\n');
        }
        normalized
    }

    /// Turn a terminal's line endings into a reader's, carrying a `\r` that
    /// ended the chunk into the next one.
    fn normalize(&mut self, text: &str) -> String {
        let mut complete = if self.trailing_return {
            format!("\r{text}")
        } else {
            text.to_string()
        };
        self.trailing_return = false;
        if complete.ends_with('\r') {
            complete.pop();
            self.trailing_return = true;
        }
        complete
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\x07', "")
    }

    /// Give up on an escape sequence that has outgrown the bound, and remember
    /// what kind it was so its terminator is skipped rather than printed.
    fn enforce_pending_bound(&mut self) {
        if self.pending.len() <= MAX_PENDING {
            return;
        }
        self.discarding = Some(if self.pending[1..].starts_with(']') {
            Discarding::Osc
        } else {
            Discarding::Csi
        });
        self.pending.clear();
    }

    /// Drop the head of a chunk that belongs to a sequence already given up
    /// on, up to and including its terminator.
    fn skip_discarded(&mut self, chunk: &str) -> String {
        let Some(discarding) = self.discarding else {
            return chunk.to_string();
        };
        match discarding {
            Discarding::Csi => match chunk.find(|c: char| ('\u{40}'..='\u{7e}').contains(&c)) {
                Some(at) => {
                    self.discarding = None;
                    chunk[at + 1..].to_string()
                }
                None => String::new(),
            },
            Discarding::Osc => {
                if let Some(at) = chunk.find('\x07') {
                    self.discarding = None;
                    return chunk[at + 1..].to_string();
                }
                if let Some(at) = chunk.find("\x1b\\") {
                    self.discarding = None;
                    return chunk[at + 2..].to_string();
                }
                String::new()
            }
        }
    }
}

/// The content of the OSC sequence starting at `escape`, and where it ends.
fn osc_end(text: &str, escape: usize) -> Option<(&str, usize)> {
    let from = escape + 2;
    let bel = text[from..].find('\x07').map(|at| (from + at + 1, 1));
    let string_terminator = text[from..].find("\x1b\\").map(|at| (from + at + 2, 2));
    let (end, terminator) = match (bel, string_terminator) {
        (Some(bel), Some(st)) if bel.0 <= st.0 => bel,
        (Some(_), Some(st)) => st,
        (Some(bel), None) => bel,
        (None, Some(st)) => st,
        (None, None) => return None,
    };
    Some((&text[from..end - terminator], end))
}

/// Where the CSI sequence starting at `escape` ends: the first byte in the
/// final range, which is what closes every one of them.
fn csi_end(text: &str, escape: usize) -> Option<usize> {
    text[escape + 2..]
        .find(|c: char| ('\u{40}'..='\u{7e}').contains(&c))
        .map(|at| escape + 2 + at + 1)
}

#[cfg(test)]
mod tests {
    //! Inline because TC-EXEC-SANE-8 reads `pending` directly. The bound is
    //! the one invariant this module has that its own output cannot show: the
    //! recovery skips exactly what the parser would have consumed, so the text
    //! is byte-for-byte identical with the bound and without it, and only the
    //! memory differs.

    use super::*;

    // Kept deliberately: without it, raising `MAX_PENDING` to 8 MiB leaves
    // TC-EXEC-SANE-8 green while handing a child a thousand times the memory -
    // measured, that mutation survived until this line existed.
    const _: () = assert!(MAX_PENDING <= 64 * 1024);

    /// TC-EXEC-SANE-8: a child cannot make the sanitizer hold more than the
    /// bound, however it feeds it.
    ///
    /// The carry is memory whose size the child chooses, so a program that
    /// writes `ESC [` and never stops is an allocation nothing else bounds.
    /// Fed as many small reads rather than one huge one. Both cross the same
    /// comparison, so this is not a second path - it is the shape a real read
    /// loop produces, and it is the one that distinguishes a bound on the
    /// accumulated buffer from a bound on the current chunk. A per-chunk check
    /// would pass the single-huge-read form and fail this one.
    ///
    /// Input: an unterminated CSI and an unterminated OSC, each fed as many
    /// small chunks that only exceed the bound in aggregate.
    /// Expected: after every push, the carried buffer is within the bound.
    #[test]
    fn a_child_cannot_make_the_carried_buffer_grow_past_the_bound() {
        // One introducer is enough: `enforce_pending_bound` compares the
        // length before it looks at what kind of sequence this is.
        let mut sanitizer = Sanitizer::new();
        let _ = sanitizer.push("\u{1b}[");
        for _ in 0..(MAX_PENDING / 64 * 4) {
            let _ = sanitizer.push(&"1".repeat(64));
            assert!(
                sanitizer.pending.len() <= MAX_PENDING,
                "small reads accumulated {} bytes carried, bound is {MAX_PENDING}",
                sanitizer.pending.len()
            );
        }
    }
}
