//! Text rules every renderer shares.
//!
//! Two families, and which one a renderer wants depends on where its text came
//! from. [`truncate`] and [`wrap`] size text the harness did not write - a
//! model's answer, a tool's result, a value out of a config file - and so they
//! [`tame`] it first. [`fit`], [`light`], [`plain`] and [`visible_width`] read
//! a line a [`Theme`](crate::Theme) has already painted, and keep the
//! sequences in it, because those are the renderer's own.

use unicode_width::UnicodeWidthChar;

use crate::color::Charset;

/// The columns a terminal draws `char` in.
///
/// One for most of what a turn writes, two for a CJK character or an emoji,
/// none for a combining mark. `unicode_width` has no answer for a control
/// character, which is none here: a renderer that met one has already lost the
/// row, and counting it as a column would leave every row after it short.
fn columns(char: char) -> usize {
    UnicodeWidthChar::width(char).unwrap_or(0)
}

/// How many leading characters of `text` a terminal draws inside `width`
/// columns.
///
/// Zero when the first character is already wider than the width. A cut takes
/// that answer as it is - the width is the promise, and a column overrun
/// corrupts every row under it - while a fold, which has to make progress or
/// never end, takes one character anyway.
fn take(text: &[char], width: usize) -> usize {
    let mut columns = 0;
    for (taken, char) in text.iter().enumerate() {
        columns += self::columns(*char);
        if columns > width {
            return taken;
        }
    }
    text.len()
}

/// The columns a terminal draws every character of `text` in.
fn span(text: &[char]) -> usize {
    text.iter().copied().map(columns).sum()
}

/// Make text the harness did not write safe to draw.
///
/// A tool's result is whatever the tool returned, and a model's answer is
/// whatever the model wrote. Sent to a terminal unchanged, either can do more
/// than be read: `ESC [ 2 J` clears the screen the frame is being drawn on,
/// `ESC ] 0 ;` renames the window, `BEL` rings, and any of them lands in the
/// middle of a page the reader is holding still. A colour written this way
/// also arrives under `--color never`, which the surface promises will write
/// none.
///
/// So an escape sequence is taken out whole - it was a command that drew
/// nothing, and nothing is what it should leave - and a stray control
/// character becomes a space, so that a byte between two words cannot join
/// them. A tab becomes the spaces that reach the next stop, counted from the
/// start of its own line: a tab drawn as a tab is a column count nothing here
/// can predict, because the stops belong to the terminal, and one drawn as a
/// single space is a column count that is predictably wrong - a Makefile, a
/// Go file and a stack trace are all indented with tabs, and squashing each
/// to one column throws away the nesting they are read by. Expanded here, the
/// terminal never sees a tab and the width is exact.
///
/// [`STOP`] columns apart, which is every terminal's default and what the
/// tools that write tabs assume.
///
/// Newlines survive. They are what [`wrap`] folds a paragraph on, and a tool
/// that wrote lines meant lines.
///
/// A tool's own colour is dropped rather than honoured. Upstream's terminal
/// card parses the sequences and draws the colours it finds; that is a reader
/// of ANSI, and this is a filter, because the family of sequences that carries
/// a colour is the family that carries a cursor move. A parser is a later
/// slice, and it would still have to end here for everything it refused.
pub fn tame(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    // Where the current line has got to, so a tab knows which stop is next.
    let mut column = 0;
    while let Some(char) = chars.next() {
        match char {
            '\n' => {
                out.push('\n');
                column = 0;
            }
            '\t' => {
                let reach = STOP - column % STOP;
                out.extend(std::iter::repeat_n(' ', reach));
                column += reach;
            }
            '\u{1b}' => skip(&mut chars),
            // C0 and DEL. Everything above them is text, including the C1
            // range, which a terminal reading UTF-8 does not act on.
            char if char.is_control() => {
                out.push(' ');
                column += 1;
            }
            char => {
                out.push(char);
                column += columns(char);
            }
        }
    }
    out
}

/// Columns between tab stops. Eight is the terminal default everywhere this
/// binary runs, and what the tools that indent with tabs are written against.
const STOP: usize = 8;

/// Make text the harness did not write safe to draw on one row.
///
/// [`tame`] keeps newlines, because a paragraph is folded on them. A row of a
/// frame is not a paragraph: a line feed inside one is written with no
/// carriage return, and every row after it lands in the wrong column. On a
/// stream it is worse than wrong - the second line arrives with none of the
/// wording that said what the first one was, so it reads as this build's own
/// words.
///
/// So the runs of blank space between the words become one space each, and
/// the ones at the ends go. A heading, a footer, a fault and a cell of a table
/// all want this; a transcript line wants [`tame`].
pub fn tame_line(text: &str) -> String {
    tame(text).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The word a renderer draws in place of a value that draws nothing.
///
/// Parenthesised, so it reads as this build's word for what is not there
/// rather than as the value itself. Two ways a value arrives with nothing in
/// it, and a reader cannot tell them apart anyway: a caller that had nothing
/// to say, and a value [`tame_line`] had to take every character out of.
///
/// A line that simply stopped where its value should be reads as one the
/// reader failed to see, and it ends in whatever blank space put the value
/// there.
pub fn or_empty(text: &str) -> &str {
    match visible_width(text) {
        0 => "(empty)",
        _ => text,
    }
}

/// Step over the rest of one escape sequence, having read its `ESC`.
///
/// Three shapes reach a terminal. `CSI` - `ESC [` - runs to a byte in `@` to
/// `~`, and is what colour, cursor movement and erasing are written as. `OSC` -
/// `ESC ]` - sets a window's title or its clipboard and runs to `BEL` or to
/// `ST`. Anything else is `ESC` and one character. A sequence this does not
/// recognise is still ended by the first of those rules that matches, which
/// is the safe way to be wrong: a filter that gave up and passed the rest
/// through would pass exactly the sequence it failed to read.
fn skip(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.next() {
        Some('[') => {
            for char in chars.by_ref() {
                if matches!(char, '@'..='~') {
                    break;
                }
            }
        }
        Some(']') => {
            while let Some(char) = chars.next() {
                if char == '\u{7}' {
                    break;
                }
                // `ST` is `ESC \`, so the escape is only the end of the
                // sequence when the character after it is the backslash.
                if char == '\u{1b}' && chars.peek() == Some(&'\\') {
                    chars.next();
                    break;
                }
            }
        }
        _ => {}
    }
}

/// Cut `text` to `width` columns, marking that it was cut.
///
/// Used by the status line, where wrapping is worse than being short - the
/// terminal scrolls and the next repaint lands on the wrong row - and by any
/// renderer showing a value it did not author, such as a tool's arguments.
pub fn truncate(text: &str, width: usize, charset: Charset) -> String {
    // Tamed before it is measured, not after: a sequence taken out afterwards
    // would already have been paid for in columns the reader never sees.
    let text = tame(text);
    let chars: Vec<char> = text.chars().collect();
    if span(&chars) <= width {
        return text;
    }
    let mark = match charset {
        Charset::Unicode => "…",
        Charset::Ascii => "...",
    };
    if width <= visible_width(mark) {
        // No room to say it was cut. The width is the harder promise: a value
        // that overruns its column corrupts every line drawn under it.
        return chars[..take(&chars, width)].iter().collect();
    }
    let keep = width - visible_width(mark);
    chars[..take(&chars, keep)]
        .iter()
        .copied()
        .chain(mark.chars())
        .collect()
}

/// The columns a terminal draws `text` in, ignoring the SGR sequences a theme
/// wrote into it.
///
/// A painted string is longer than it looks: `\x1b[1mai\x1b[0m` is two columns
/// and eleven characters. Any renderer that pads, cuts or counts a line it did
/// not compose itself has to ask this rather than `chars().count()`.
pub fn visible_width(text: &str) -> usize {
    let mut columns = 0;
    let mut chars = text.chars();
    while let Some(char) = chars.next() {
        if char != '\u{1b}' {
            columns += self::columns(char);
            continue;
        }
        // Every escape a `Theme` writes is `ESC [ ... m`. Skipping to the `m`
        // is enough for those and harmless for the rest.
        for escape in chars.by_ref() {
            if escape == 'm' {
                break;
            }
        }
    }
    columns
}

/// The text a terminal draws, with the SGR sequences a theme wrote taken out.
///
/// [`visible_width`] counts what is drawn; this returns it. A renderer that
/// searches, sorts or compares a line it did not compose has to ask for this
/// first: a painted line holds escape codes between its words, so `contains`
/// against the painted form answers no to questions whose answer is yes.
pub fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(char) = chars.next() {
        if char != '\u{1b}' {
            out.push(char);
            continue;
        }
        // The same rule as `visible_width`: every escape a `Theme` writes is
        // `ESC [ ... m`, and skipping to the `m` is enough for those.
        for escape in chars.by_ref() {
            if escape == 'm' {
                break;
            }
        }
    }
    out
}

/// Cut a possibly painted `text` to `width` columns.
///
/// [`truncate`] counts characters, so it would spend the width on escape
/// sequences and cut a line that fits - or cut one in the middle of an escape
/// and leave the terminal painted for the rest of the session. This one counts
/// what is drawn, keeps the sequences it passes, and closes with a reset when
/// it cut a painted line.
pub fn fit(text: &str, width: usize, charset: Charset) -> String {
    if visible_width(text) <= width {
        return text.to_string();
    }
    let mark = match charset {
        Charset::Unicode => "…",
        Charset::Ascii => "...",
    };
    // The same rule as `truncate`: with no room for the mark, the width is
    // still the harder promise.
    let (keep, mark) = if width <= visible_width(mark) {
        (width, "")
    } else {
        (width - visible_width(mark), mark)
    };

    let mut out = String::new();
    let mut columns = 0;
    let mut painted = false;
    let mut chars = text.chars();
    while let Some(char) = chars.next() {
        if char == '\u{1b}' {
            painted = true;
            out.push(char);
            for escape in chars.by_ref() {
                out.push(escape);
                if escape == 'm' {
                    break;
                }
            }
            continue;
        }
        // Asked before the character is written, not after: a two-column
        // character taken while one column is left would put the mark past
        // the width, which is the one thing this must not do.
        let drawn = self::columns(char);
        if columns + drawn > keep {
            break;
        }
        out.push(char);
        columns += drawn;
    }
    out.push_str(mark);
    if painted {
        out.push_str("\u{1b}[0m");
    }
    out
}

/// Fold `text` to `width` columns, breaking between words.
///
/// Prose a model wrote is the one thing on the page whose length the renderer
/// does not control. Left to the terminal, its continuation starts in column
/// zero, under the label column everything else aligns to, and a paragraph
/// stops looking like it belongs to the speaker who said it. Folding here lets
/// the caller indent the rest itself.
///
/// Newlines in `text` are kept, blank lines included. A word too long for any
/// line - a path, a URL, a base64 blob - is broken rather than allowed to
/// overrun.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let text = tame(text);
    let mut lines = Vec::new();

    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut filled = 0;

        for word in paragraph.split_whitespace() {
            let mut rest: Vec<char> = word.chars().collect();
            while span(&rest) > width {
                if filled > 0 {
                    lines.push(std::mem::take(&mut line));
                    filled = 0;
                }
                // One character in any case: a character wider than the whole
                // width overruns it by a column, and a fold that took none
                // would fold this word for the rest of the run.
                let cut = take(&rest, width).max(1);
                lines.push(rest.drain(..cut).collect::<String>());
            }

            let drawn = span(&rest);
            if filled > 0 && filled + 1 + drawn > width {
                lines.push(std::mem::take(&mut line));
                filled = 0;
            }
            if filled > 0 {
                line.push(' ');
                filled += 1;
            }
            filled += drawn;
            line.extend(rest);
        }
        lines.push(line);
    }
    lines
}

/// Reverse video on, and reverse video off.
///
/// Written here as codes rather than asked of a `Theme`, which is the one
/// place in the crate that happens: a mark inside a line the theme has already
/// painted has to end without ending what it interrupted. `27` turns reverse
/// off and touches nothing else, where a theme's reset closes every attribute
/// on the line and every word after the match would come back plain.
const MARK: (&str, &str) = ("\u{1b}[7m", "\u{1b}[27m");

/// Mark every place `word` is drawn in a painted `line`.
///
/// A search that moves the window to the line holding a word has said which
/// line, not where on it. On the long lines a turn writes - a prompt, an
/// answer, a tool's arguments - that is the difference between finding a word
/// and starting to look for it.
///
/// Reverse video rather than a colour, because the line already has colours of
/// its own and a mark that competed with them would be one more thing to read.
/// It is also the one attribute that means the same on every palette: a
/// terminal's own selection looks like this.
///
/// Folded the way a search folds, so whatever the terminal draws is marked in
/// any case. A `word` that is empty, or that the line does not hold, gives the
/// line back character for character - and so does a caller with colour off,
/// who has no business writing escapes at all and should not call this.
pub fn light(line: &str, word: &str) -> String {
    let wanted = word.to_lowercase();
    if wanted.is_empty() {
        return line.to_string();
    }

    // What the terminal draws, folded, beside where each byte of it came from.
    // A match is found in what is drawn and marked in what was given, so the
    // two have to be built together: an escape between two letters is no gap
    // on the screen and five bytes in the string.
    let (mut drawn, mut from, mut upto) = (String::new(), Vec::new(), Vec::new());
    let mut chars = line.char_indices();
    while let Some((at, char)) = chars.next() {
        if char != '\u{1b}' {
            drawn.extend(char.to_lowercase());
            from.resize(drawn.len(), at);
            upto.resize(drawn.len(), at + char.len_utf8());
            continue;
        }
        // The same rule as `plain`: every escape a `Theme` writes is
        // `ESC [ ... m`, and it is drawn as nothing at all.
        for (_, escape) in chars.by_ref() {
            if escape == 'm' {
                break;
            }
        }
    }

    let mut out = String::new();
    let mut written = 0;
    for (hit, _) in drawn.match_indices(&wanted) {
        let (start, end) = (from[hit], upto[hit + wanted.len() - 1]);
        out.push_str(&line[written..start]);
        out.push_str(MARK.0);
        out.push_str(&armed(&line[start..end]));
        out.push_str(MARK.1);
        written = end;
    }
    out.push_str(&line[written..]);
    out
}

/// Copy a stretch of a painted line, arming the mark again after every escape
/// in it.
///
/// A word can be painted in the middle - `tool` and its name are one match and
/// two colours - and the sequence between them may be a reset, which would
/// take the mark off halfway through the word it is marking. Cheaper to arm it
/// again after every sequence than to work out which ones would have mattered.
fn armed(span: &str) -> String {
    let mut out = String::new();
    let mut chars = span.chars();
    while let Some(char) = chars.next() {
        out.push(char);
        if char != '\u{1b}' {
            continue;
        }
        for escape in chars.by_ref() {
            out.push(escape);
            if escape == 'm' {
                break;
            }
        }
        out.push_str(MARK.0);
    }
    out
}
