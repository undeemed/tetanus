//! The key map, on a screen of its own.
//!
//! Every full-screen view in this binary tells the reader what it answers on
//! one row of a footer, and every slice that added a key made that row longer.
//! A journal now scrolls six ways, searches, walks the matches, and closes -
//! which is more than a footer can say on a narrow terminal without being cut
//! mid-word. So the footer keeps the keys a reader needs while they are
//! reading, `?` spells out the rest, and this module is the frame that does
//! the spelling.
//!
//! # Stakeholders and concerns
//!
//! - *A reader who does not know the keys*: what can I press here, and how do
//!   I get back to what I was looking at?
//! - *A reader on an 80x24 terminal, or smaller*: is the footer still legible
//!   once the view has more keys than fit on it?
//! - *The presentation lane*: is there one key card, or one per view?
//!
//! # Composition
//!
//! ```text
//! card ── the frame: a heading, a row per key, and how to get back
//! ```
//!
//! Each view owns its own table, because each view knows its own keys, and a
//! table kept here would be a second place to change every time one of them
//! gains a key. What is shared is the shape: the same furniture as
//! [`Page`](tetanus_ui::Page), so the card lands where the page was rather
//! than shifting every row under the reader.
//!
//! # Rationale: any key goes back
//!
//! Not `?` again, and not `q`. A reader who opened the card by accident wants
//! out of it, and a card that answers one key of the many they might try is a
//! view they have to read their way out of. Nothing here acts on a key, so
//! there is nothing for the wrong one to do.

use tetanus_ui::{bar, visible_width, Frame, Role, Theme};

use super::browse::NAME;

/// Rows the card spends on furniture: the heading, a blank under it, a blank
/// over the footer, and the footer. The same four
/// [`Page`](tetanus_ui::Page) spends, so a card drawn in place of a page has
/// its rows in the same places.
const CHROME: usize = 4;

/// Columns between the keys and what they do.
const GAP: usize = 2;

/// One row of a key map: which keys, and what they do.
pub type Row = (&'static str, &'static str);

/// The keys of one view, spelled out on a whole screen.
///
/// `what` names the view in the heading, so a reader who pressed `?` inside a
/// journal opened from the session list can see which of the two they are
/// looking at.
///
/// Rows past the bottom of a short terminal are counted rather than dropped in
/// silence: a card that quietly stopped listing would be a worse answer than
/// the footer it replaced.
pub fn card(theme: &Theme, cols: usize, rows: usize, what: &str, keys: &[Row]) -> Frame {
    let room = rows.saturating_sub(CHROME);
    let width = keys.iter().map(|(key, _)| visible_width(key)).max();
    let mut frame = Frame::new(cols, rows);
    frame.row(bar(
        cols,
        &theme.paint(Role::Heading, NAME).to_string(),
        &theme
            .paint(Role::Muted, &format!("{what} keys"))
            .to_string(),
    ));
    frame.blank();
    // One row of the card is spent saying how many rows the card is missing,
    // so the cut is made a row earlier than the room allows.
    let shown = if keys.len() > room {
        room.saturating_sub(1)
    } else {
        keys.len()
    };
    for (key, does) in keys.iter().take(shown) {
        let pad = " ".repeat(width.unwrap_or(0) - visible_width(key) + GAP);
        frame.row(format!(
            "  {}{pad}{}",
            theme.paint(Role::Accent, key),
            theme.paint(Role::Muted, does)
        ));
    }
    if let Some(left) = keys.len().checked_sub(shown).filter(|left| *left > 0) {
        let more = format!(
            "{} and {left} more, on a taller screen",
            theme.glyph("…", "...")
        );
        frame.row(format!("  {}", theme.paint(Role::Muted, &more)));
    }
    while frame.free() > 1 {
        frame.blank();
    }
    frame.row(bar(
        cols,
        &theme.paint(Role::Muted, "any key goes back").to_string(),
        "",
    ));
    frame
}

/// Test Design Specification: the key card.
///
/// Features tested: that a card holds one row per key with the keys in a
/// column of their own, headed by the view it belongs to and footed by the way
/// out of it; and that a card with more keys than a short terminal has rows
/// says how many it could not show rather than dropping them in silence.
///
/// Features NOT tested here: which keys a view has and what they do (owned by
/// each view, asserted by TC-CLI-BROWSE-10 and TC-CLI-PICK-12), the exact
/// height of a frame and the cut to the width (owned by `tetanus_ui::Frame`),
/// and the colour of a row (owned by `tetanus_ui::Theme`).
///
/// Environmental needs: none. Every case composes into a buffer at a size it
/// states. No case opens a terminal.
#[cfg(test)]
mod tests {
    use tetanus_ui::{buffered, Charset};

    use super::*;

    const COLS: usize = 60;

    fn theme() -> Theme {
        Theme::new(false, Charset::Unicode)
    }

    /// One frame, as rows of text, with the terminal's own control codes gone.
    fn rows(frame: &Frame, cols: usize) -> Vec<String> {
        let mut ui = buffered(theme(), cols);
        frame.paint(&mut ui).expect("paint");
        ui.contents()
            .trim_start_matches("\x1b[H")
            .trim_end_matches("\x1b[J")
            .split("\r\n")
            .map(|row| row.trim_end_matches("\x1b[K").trim_end().to_string())
            .collect()
    }

    const MAP: [Row; 3] = [
        ("↑ ↓", "one line back, one line on"),
        ("pgup pgdn", "a screenful either way"),
        ("?", "this card; any key goes back"),
    ];

    /// TC-CLI-KEYS-1: three keys on a screen with room for all of them.
    /// Expected: the frame is the height asked for; the heading names the view
    /// and the footer says any key goes back; every key is on a row of its
    /// own, and every description starts in the same column, which is what
    /// makes a card readable at a glance rather than a paragraph.
    #[test]
    fn a_card_is_a_column_of_keys_and_a_way_back() {
        let frame = card(&theme(), COLS, 10, "journal", &MAP);
        let rows = rows(&frame, COLS);

        assert_eq!(rows.len(), 10, "not the height asked for: {rows:?}");
        assert!(rows[0].starts_with("tetanus"), "not headed: {rows:?}");
        assert!(rows[0].ends_with("journal keys"), "not named: {rows:?}");
        assert_eq!(rows[9], "any key goes back", "no way back: {rows:?}");

        let said: Vec<usize> = MAP
            .iter()
            .map(|(_, does)| {
                let row = rows
                    .iter()
                    .find(|row| row.contains(does))
                    .unwrap_or_else(|| panic!("{does} is on no row: {rows:?}"));
                // Columns, not bytes: an arrow is three bytes wide and one
                // column, which is the whole reason the padding is measured
                // the way it is.
                visible_width(&row[..row.find(does).expect("found above")])
            })
            .collect();
        assert_eq!(
            said,
            vec![said[0]; MAP.len()],
            "the descriptions do not line up: {rows:?}"
        );
    }

    /// TC-CLI-KEYS-2: a card with more keys than the screen has rows.
    /// Expected: what fits is shown, and the last row counts what does not. A
    /// card that quietly stopped listing would be a worse answer than the
    /// footer it replaced, because the reader cannot tell the difference
    /// between a key that is missing and a key that does not exist.
    #[test]
    fn a_short_screen_counts_the_keys_it_could_not_show() {
        // Four rows of furniture and six rows of screen leaves two: one key,
        // and the row that says the other two are elsewhere.
        let frame = card(&theme(), COLS, 6, "journal", &MAP);
        let rows = rows(&frame, COLS);

        assert!(
            rows.iter().any(|row| row.contains("one line back")),
            "the first key was dropped: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("and 2 more")),
            "the dropped keys are not counted: {rows:?}"
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.contains("any key goes back") && row.contains("this card")),
            "a key row was written over the footer: {rows:?}"
        );
    }
}
