//! Test Design Specification: the styled writer and the palette.
//!
//! Features tested: that a plain theme emits no escape bytes; that a colored
//! theme wraps a span and resets it; that format width pads the text and not
//! the escape sequences; that a field pads its label in the columns a
//! terminal draws it in; what a field says when its value draws nothing; the
//! diagnostic tags; the charset-dependent glyphs.
//!
//! Features NOT tested here: the policy that produced the theme (see
//! `color_policy.rs`) and the real process streams.
//!
//! Environmental needs: none. Every case writes into a `Vec<u8>`.

use tetanus_ui::{buffered, tame_line, visible_width, Charset, Role, Theme};

const ESC: char = '\u{1b}';

/// TC-UI-WRITE-1: a plain theme.
/// Expected: the exact text, no escape byte anywhere, and the trailing newline
/// the caller expects from a line writer.
#[test]
fn a_plain_theme_writes_bytes_a_pipe_can_keep() {
    let mut ui = buffered(Theme::plain(), 80);
    ui.line("turn 1").expect("write");
    ui.field("stop", 7, "natural").expect("write");
    ui.heading("outcome").expect("write");

    let out = ui.contents();
    assert_eq!(out, "turn 1\nstop     natural\n\noutcome\n");
    assert!(!out.contains(ESC), "{out:?}");
}

/// TC-UI-WRITE-2: the same calls with color on.
/// Expected: the payload text is unchanged and every styled span is reset, so
/// nothing bleeds into the next line.
#[test]
fn a_colored_theme_wraps_and_resets_every_span() {
    let mut ui = buffered(Theme::new(true, Charset::Unicode), 80);
    ui.heading("outcome").expect("write");
    ui.field("stop", 7, "natural").expect("write");

    let out = ui.contents();
    assert!(out.contains("outcome"), "{out:?}");
    assert!(out.contains("natural"), "{out:?}");
    assert!(out.contains(ESC), "styling was requested but not emitted");
    assert_eq!(
        out.matches("\u{1b}[0m").count(),
        2,
        "one reset per styled span: {out:?}"
    );
    assert!(out.ends_with('\n'));
}

/// TC-UI-WRITE-3: format width on a colored span.
/// Expected: the padding lands on the text, so a colored column lines up with
/// a plain one byte for byte once the escapes are stripped.
#[test]
fn padding_measures_the_text_not_the_escapes() {
    let colored = Theme::new(true, Charset::Unicode);
    let painted = format!("[{:<8}]", colored.paint(Role::Tool, "echo"));
    let stripped = painted.replace("\u{1b}[35m", "").replace("\u{1b}[0m", "");
    assert_eq!(stripped, format!("[{:<8}]", "echo"));
    assert_eq!(stripped, "[echo    ]");
}

/// TC-UI-WRITE-4: the three diagnostic shapes.
/// Expected: `note: `, `warning: `, `error: ` prefixes, the tags a Rust user
/// already reads, with the message verbatim after them.
#[test]
fn diagnostics_carry_the_familiar_tags() {
    let mut ui = buffered(Theme::plain(), 80);
    ui.note("try --adapter mock").expect("write");
    ui.warn("the journal already existed").expect("write");
    ui.error("DEEPSEEK_API_KEY is not set").expect("write");

    assert_eq!(
        ui.contents(),
        "note: try --adapter mock\n\
         warning: the journal already existed\n\
         error: DEEPSEEK_API_KEY is not set\n"
    );
}

/// TC-UI-WRITE-5: charset-dependent drawing.
/// Expected: a Unicode theme rules with `─`, an ASCII theme with `-`, and both
/// rules are exactly as wide as the configured width.
#[test]
fn a_rule_follows_the_charset_and_the_width() {
    let mut unicode = buffered(Theme::new(false, Charset::Unicode), 12);
    unicode.rule().expect("write");
    assert_eq!(unicode.contents(), "────────────\n");

    let mut ascii = buffered(Theme::plain(), 12);
    ascii.rule().expect("write");
    assert_eq!(ascii.contents(), "------------\n");
}

/// TC-UI-WRITE-6: a label in a script a terminal draws twice as wide.
/// Expected: the value starts in the same column as it does beside a label of
/// the same width in Latin characters. `field` pads the label itself, because
/// a format width counts the characters of a label and would leave the wide
/// row short by one column for every character of it.
#[test]
fn a_field_pads_its_label_in_the_columns_it_draws() {
    let mut ui = buffered(Theme::plain(), 80);
    ui.field("\u{65e5}\u{672c}\u{8a9e}", 9, "wide")
        .expect("write");
    ui.field("log.level", 9, "trace").expect("write");

    let out = ui.contents();
    for (line, value) in out.lines().zip(["wide", "trace"]) {
        let at = line.find(value).expect(value);
        assert_eq!(
            visible_width(&line[..at]),
            11,
            "the value does not start where the other one does: {line:?}"
        );
    }
}

/// TC-UI-WRITE-7: a field whose value draws nothing - empty, and every
/// character taken out of it by a filter.
/// Expected: both rows say `(empty)` where the value would be, and neither
/// ends in blank space. A row that stopped after its label would read as a
/// value the reader failed to see, and it would leave the gap hanging off the
/// end of the line.
#[test]
fn a_value_that_draws_nothing_is_said_and_not_left_blank() {
    let mut ui = buffered(Theme::plain(), 80);
    ui.field("journal", 7, "").expect("write");
    ui.field("model", 7, &tame_line("\u{1b}[2J\u{1b}]0;pwned\u{7}"))
        .expect("write");

    let out = ui.contents();
    assert_eq!(out, "journal  (empty)\nmodel    (empty)\n");
    for line in out.lines() {
        assert_eq!(
            line.trim_end(),
            line,
            "the row ends in blank space: {line:?}"
        );
    }
}
