//! Test Design Specification: the text rules every renderer shares.
//!
//! Features tested: cutting a value to a width and saying so, and folding a
//! paragraph to a width without losing a word. Features NOT tested here: what
//! any particular renderer does with the result - the status line owns its own
//! cases in `progress.rs`, the timeline owns its own in `render/timeline.rs`.
//!
//! Environmental needs: none. Every case is a pure function of its input.

use tetanus_ui::{truncate, wrap, Charset};

/// TC-UI-TEXT-1: a value that already fits.
/// Expected: returned unchanged, with no mark added. A renderer must be able
/// to call this on every value without marking the ones it did not cut.
#[test]
fn what_fits_is_left_alone() {
    assert_eq!(truncate("echo", 10, Charset::Unicode), "echo");
    assert_eq!(truncate("echo", 4, Charset::Unicode), "echo");
}

/// TC-UI-TEXT-2: a value that does not fit, in each charset.
/// Expected: the result occupies exactly the width, and the last characters
/// are the ellipsis the charset allows - one column of it, or three.
#[test]
fn what_does_not_fit_is_cut_and_marked() {
    let long = "deepseek-v4-pro";

    let unicode = truncate(long, 8, Charset::Unicode);
    assert_eq!(unicode, "deepsee…");
    assert_eq!(unicode.chars().count(), 8);

    let ascii = truncate(long, 8, Charset::Ascii);
    assert_eq!(ascii, "deeps...");
    assert_eq!(ascii.chars().count(), 8);
}

/// TC-UI-TEXT-3: a width too narrow to hold the mark itself.
/// Expected: the width is still honoured. A one-column column is nonsense, but
/// a renderer that overruns its box by three columns corrupts every line under
/// it, so the mark is what gives way.
#[test]
fn a_width_narrower_than_the_mark_still_holds() {
    assert_eq!(truncate("deepseek", 2, Charset::Ascii), "de");
    assert_eq!(truncate("deepseek", 1, Charset::Unicode), "d");
    assert_eq!(truncate("deepseek", 0, Charset::Unicode), "");
}

/// TC-UI-TEXT-4: a paragraph longer than the line.
/// Expected: folded between words, every line inside the width, and not one
/// word lost or duplicated.
#[test]
fn a_paragraph_folds_between_words() {
    let text = "the agent claims your prompt, assembles a prompt and a tool catalogue";
    let lines = wrap(text, 24);

    for line in &lines {
        assert!(line.chars().count() <= 24, "`{line}` overruns 24");
    }
    assert_eq!(lines.join(" "), text);
}

/// TC-UI-TEXT-5: text that already has newlines in it.
/// Expected: they are kept, blank lines included. A model that wrote two
/// paragraphs meant two paragraphs.
#[test]
fn existing_newlines_survive() {
    assert_eq!(
        wrap("first\n\nsecond", 20),
        vec!["first".to_string(), String::new(), "second".to_string()]
    );
}

/// TC-UI-TEXT-6: a word no line can hold.
/// Expected: it is broken at the width rather than allowed to overrun, and the
/// pieces still spell the original. A path or a URL is not a failure case.
#[test]
fn a_word_too_long_for_the_line_is_broken() {
    let lines = wrap("see /very/long/path/to/somewhere/deep/inside now", 10);

    for line in &lines {
        assert!(line.chars().count() <= 10, "`{line}` overruns 10");
    }
    assert_eq!(
        lines.concat().replace(' ', ""),
        "see/very/long/path/to/somewhere/deep/insidenow"
    );
}
