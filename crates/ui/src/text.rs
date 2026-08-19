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
