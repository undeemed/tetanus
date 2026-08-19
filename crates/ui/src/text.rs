//! Text rules every renderer shares.

use crate::color::Charset;

/// Cut `text` to `width` columns, marking that it was cut.
///
/// Used by the status line, where wrapping is worse than being short - the
/// terminal scrolls and the next repaint lands on the wrong row - and by any
/// renderer showing a value it did not author, such as a tool's arguments.
pub fn truncate(text: &str, width: usize, charset: Charset) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mark = match charset {
        Charset::Unicode => "…",
        Charset::Ascii => "...",
    };
    if width <= mark.chars().count() {
        // No room to say it was cut. The width is the harder promise: a value
        // that overruns its column corrupts every line drawn under it.
        return text.chars().take(width).collect();
    }
    let keep = width - mark.chars().count();
    text.chars().take(keep).chain(mark.chars()).collect()
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
            columns += 1;
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
    let keep = width.saturating_sub(mark.chars().count());
    let (keep, mark) = if width <= mark.chars().count() {
        (width, "")
    } else {
        (keep, mark)
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
        if columns == keep {
            break;
        }
        out.push(char);
        columns += 1;
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
    let mut lines = Vec::new();

    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut filled = 0;

        for word in paragraph.split_whitespace() {
            let mut rest: Vec<char> = word.chars().collect();
            while rest.len() > width {
                if filled > 0 {
                    lines.push(std::mem::take(&mut line));
                    filled = 0;
                }
                lines.push(rest.drain(..width).collect::<String>());
            }

            if filled > 0 && filled + 1 + rest.len() > width {
                lines.push(std::mem::take(&mut line));
                filled = 0;
            }
            if filled > 0 {
                line.push(' ');
                filled += 1;
            }
            filled += rest.len();
            line.extend(rest);
        }
        lines.push(line);
    }
    lines
}
