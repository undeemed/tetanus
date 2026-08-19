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
    let keep = width.saturating_sub(mark.chars().count());
    text.chars().take(keep).chain(mark.chars()).collect()
}
