//! Test Design Specification: the text rules every renderer shares.
//!
//! Features tested: cutting a value to a width and saying so, folding a
//! paragraph to a width without losing a word, measuring and cutting a line
//! that a theme has already painted, and making text the harness did not write
//! safe to draw, as a paragraph and as one row. Features NOT tested here: what any particular renderer does
//! with the result - the status line owns its own cases in `progress.rs`, the
//! timeline owns its own in `render/timeline.rs`.
//!
//! Environmental needs: none. Every case is a pure function of its input.

use tetanus_ui::{fit, light, plain, tame, tame_line, truncate, visible_width, wrap, Charset};

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

/// TC-UI-TEXT-7: the width of a painted line.
/// Expected: the columns a terminal draws, not the characters the string
/// holds. Every alignment bug this lane has shipped so far came from counting
/// the escapes, so the measurement is a case of its own.
#[test]
fn a_painted_line_is_measured_by_what_is_drawn() {
    assert_eq!(visible_width("plain"), 5);
    assert_eq!(visible_width("\u{1b}[1mbold\u{1b}[0m"), 4);
    assert_eq!(visible_width(""), 0);
}

/// TC-UI-TEXT-8: cutting a painted line.
/// Expected: the visible text is cut to the width, the sequences that opened
/// the colour are kept, and a reset closes the line. A cut that dropped the
/// reset would leave every row under it painted.
#[test]
fn cutting_a_painted_line_keeps_its_colour_and_closes_it() {
    let painted = "\u{1b}[1mfour five six\u{1b}[0m";
    let cut = fit(painted, 6, Charset::Unicode);

    assert_eq!(visible_width(&cut), 6);
    assert!(cut.starts_with("\u{1b}[1m"), "{cut:?}");
    assert!(cut.ends_with("\u{1b}[0m"), "{cut:?}");
    assert!(cut.contains("four …"), "{cut:?}");
}

/// TC-UI-TEXT-9: what `fit` leaves alone, and the ASCII mark.
/// Expected: a painted line that already fits is returned character for
/// character, so a frame that changed nothing writes the same bytes; and an
/// ASCII terminal is cut with the mark it can draw.
#[test]
fn fit_leaves_a_line_that_fits_and_marks_an_ascii_cut() {
    let painted = "\u{1b}[1mshort\u{1b}[0m";
    assert_eq!(fit(painted, 40, Charset::Unicode), painted);
    assert_eq!(fit("four five six", 8, Charset::Ascii), "four ...");
}

/// TC-UI-TEXT-10: the visible text of a painted line.
/// Expected: the characters the terminal draws, in order, with the sequences
/// gone and nothing else touched. A renderer that searches a line it did not
/// compose asks for this first: two words either side of a colour change are
/// one string on the screen and two with an escape between them in memory, and
/// a search that could not find them would be a search nobody could trust.
#[test]
fn a_painted_line_reads_back_as_what_it_draws() {
    assert_eq!(plain("plain"), "plain");
    assert_eq!(plain("\u{1b}[1mbold\u{1b}[0m"), "bold");
    assert_eq!(plain("\u{1b}[36mtool\u{1b}[0m echo"), "tool echo");
    assert_eq!(plain(""), "");
}

/// TC-UI-TEXT-11: every place a word is drawn on a plain line.
/// Expected: each occurrence is wrapped in the mark, whatever case it was
/// written in, and nothing else about the line changes - what the terminal
/// draws, and how wide it draws it, are the same before and after. A mark that
/// moved a column would move every line under it on the next frame.
#[test]
fn a_word_is_marked_everywhere_it_is_drawn() {
    let line = "echo said Echo, and echoed";
    let lit = light(line, "echo");

    assert_eq!(lit.matches("\u{1b}[7m").count(), 3, "{lit:?}");
    assert_eq!(lit.matches("\u{1b}[27m").count(), 3, "{lit:?}");
    assert!(lit.starts_with("\u{1b}[7mecho\u{1b}[27m said"), "{lit:?}");
    assert!(lit.contains("\u{1b}[7mEcho\u{1b}[27m,"), "{lit:?}");
    assert_eq!(plain(&lit), line);
    assert_eq!(visible_width(&lit), visible_width(line));
}

/// TC-UI-TEXT-12: a word drawn inside a line the theme has already painted.
/// Expected: the mark closes with `27` and never with a full reset, so the
/// colour the rest of the line is wearing survives it; and a match with a
/// sequence inside it - a word half of which the theme coloured - is marked
/// all the way through rather than up to that sequence.
#[test]
fn a_mark_does_not_end_the_colour_it_interrupts() {
    let painted = "\u{1b}[36mtopic\u{1b}[0m turn/start";
    let lit = light(painted, "turn");
    assert!(lit.contains("\u{1b}[7mturn\u{1b}[27m/start"), "{lit:?}");
    assert!(!lit.contains("\u{1b}[7mturn\u{1b}[0m"), "{lit:?}");
    assert_eq!(plain(&lit), "topic turn/start");

    // `tool` and `echo` are one word on the screen and two colours in memory.
    let split = "\u{1b}[35mto\u{1b}[0mol";
    let lit = light(split, "tool");
    // The mark opens on the first letter, not before the colour that letter is
    // drawn in: a mark placed ahead of a sequence marks nothing.
    assert!(lit.starts_with("\u{1b}[35m\u{1b}[7mto"), "{lit:?}");
    assert!(lit.ends_with("\u{1b}[27m"), "{lit:?}");
    // Armed again after the reset in the middle, or the second half of the
    // word would be drawn unmarked.
    assert!(lit.contains("\u{1b}[0m\u{1b}[7mol"), "{lit:?}");
    assert_eq!(plain(&lit), "tool");
}

/// TC-UI-TEXT-13: nothing to mark.
/// Expected: the line back byte for byte. A renderer calls this on every line
/// of a page, and the ones the search did not find must be the same bytes the
/// frame before them held.
#[test]
fn a_line_with_nothing_to_mark_is_left_alone() {
    let painted = "\u{1b}[1mturn/start\u{1b}[0m";
    assert_eq!(light(painted, "tool"), painted);
    assert_eq!(light(painted, ""), painted);
    assert_eq!(light("", "echo"), "");
}

/// TC-UI-TEXT-14: what a terminal draws a character in, rather than how many
/// characters there are.
/// Expected: a CJK character measures two columns and an emoji two, a
/// combining mark none, and a painted wide line measures what it draws. A
/// renderer that counted characters here would compose a row that overruns
/// every box it is put in, and the row under it would land in the wrong
/// column for the rest of the screen.
#[test]
fn width_is_measured_in_columns() {
    assert_eq!(visible_width("ai"), 2);
    assert_eq!(visible_width("日本"), 4);
    assert_eq!(visible_width("🔥"), 2);
    // e + a combining acute is one column, whatever it is made of.
    assert_eq!(visible_width("e\u{301}"), 1);
    assert_eq!(visible_width("\u{1b}[1m日本\u{1b}[0m"), 4);
}

/// TC-UI-TEXT-15: prose in a script a terminal draws twice as wide.
/// Expected: no folded line is wider than the width it was folded to, and the
/// text survives the fold character for character. This is the case the
/// journal met: a Japanese prompt folded by character count is drawn at twice
/// the width of the frame holding it, so the terminal folds it again where the
/// renderer did not mean it to and every row after it is out of place.
#[test]
fn wide_prose_folds_to_the_columns_it_is_given() {
    let prose = "日本語のとても長い行をここに置きます。折り返しの幅を見ます。";

    for width in [8, 20, 40] {
        let folded = wrap(prose, width);
        for line in &folded {
            assert!(
                visible_width(line) <= width,
                "`{line}` is {} columns at width {width}",
                visible_width(line)
            );
        }
        assert_eq!(
            folded.concat(),
            prose,
            "the fold lost text at width {width}"
        );
    }
}

/// TC-UI-TEXT-16: a wide character at the width.
/// Expected: neither cut spends more columns than it was given, and neither
/// splits a character in half to do it. The character before the boundary is
/// dropped whole - a half of a two-column character is a byte sequence the
/// terminal draws as a replacement glyph, which is wider than the column the
/// cut was protecting.
#[test]
fn a_cut_lands_between_characters_not_inside_one() {
    // Six columns of text, cut to five: the mark takes one, so two characters
    // fit and the third is dropped whole rather than halved.
    assert_eq!(truncate("日本語", 5, Charset::Unicode), "日本…");
    assert_eq!(visible_width(&truncate("日本語", 5, Charset::Unicode)), 5);

    // A width the wide character cannot land on exactly: short, never over.
    // At one column nothing fits at all, which is the honest answer.
    assert_eq!(truncate("日本語", 1, Charset::Unicode), "");
    for width in 1..8 {
        let cut = truncate("日本語", width, Charset::Unicode);
        assert!(
            visible_width(&cut) <= width,
            "`{cut}` overruns {width} columns"
        );
        let painted = fit("\u{1b}[1m日本語\u{1b}[0m", width, Charset::Unicode);
        assert!(
            visible_width(&painted) <= width,
            "`{painted}` overruns {width} columns"
        );
    }
}

/// TC-UI-TEXT-17: an escape sequence in text the harness did not write.
/// Expected: taken out whole, in each of the three shapes a terminal reads,
/// with the words either side of it kept and no fragment of the sequence left
/// behind. A tool returns whatever it returns, and a result drawn unchanged
/// can clear the screen it is being drawn on, rename the window, or paint a
/// colour under `--color never`.
#[test]
fn a_sequence_a_tool_wrote_is_taken_out_whole() {
    // CSI: colour, and erasing the display.
    assert_eq!(tame("safe \u{1b}[31mred\u{1b}[0m end"), "safe red end");
    assert_eq!(tame("before \u{1b}[2J\u{1b}[H after"), "before  after");
    // OSC, ended either way it is allowed to end.
    assert_eq!(tame("a \u{1b}]0;title\u{7}b"), "a b");
    assert_eq!(tame("a \u{1b}]0;title\u{1b}\\b"), "a b");
    // Two characters, and a sequence with nothing after it.
    assert_eq!(tame("a \u{1b}7b"), "a b");
    assert_eq!(tame("a \u{1b}[31m"), "a ");
    // Nothing of the escape survives to be drawn.
    for text in ["\u{1b}[31mred", "\u{1b}]0;t\u{7}", "\u{1b}[2J"] {
        assert!(!tame(text).contains('\u{1b}'), "{text:?}");
        assert!(!tame(text).contains('['), "{text:?}");
    }
}

/// TC-UI-TEXT-18: control characters that are not part of a sequence.
/// Expected: each becomes one space, so a byte between two words cannot join
/// them; newlines survive, because they are what a paragraph is folded on and
/// a tool that wrote lines meant lines.
#[test]
fn a_stray_control_becomes_a_space_and_a_newline_survives() {
    assert_eq!(tame("one\rtwo"), "one two");
    assert_eq!(tame("ring\u{7}ing"), "ring ing");
    assert_eq!(tame("a\u{0}b\u{7f}c"), "a b c");
    assert_eq!(tame("one\ttwo"), "one two");
    assert_eq!(tame("first\n\nsecond"), "first\n\nsecond");
    assert_eq!(tame("plain text"), "plain text");
}

/// TC-UI-TEXT-19: the width rules on text a tool wrote.
/// Expected: both tame before they measure. A sequence counted as characters
/// and taken out afterwards would have been paid for in columns the reader
/// never sees, so the value would come back short and the cut would land in
/// the wrong place.
#[test]
fn the_width_rules_measure_what_will_be_drawn() {
    // Eleven characters of sequence in front of four of text.
    let painted = "\u{1b}[31mfour\u{1b}[0m five six";
    assert_eq!(truncate(painted, 8, Charset::Unicode), "four fi…");
    assert_eq!(truncate(painted, 13, Charset::Unicode), "four five six");
    assert_eq!(wrap(painted, 5), vec!["four", "five", "six"]);
    // And a sequence cannot survive either of them.
    assert!(!truncate(painted, 40, Charset::Unicode).contains('\u{1b}'));
    assert!(!wrap(painted, 40).concat().contains('\u{1b}'));
}

/// TC-UI-TEXT-20: the same text, tamed for one row of a frame.
/// Expected: every sequence and control gone as in TC-UI-TEXT-17 and
/// TC-UI-TEXT-18, and additionally no newline, no run of blank space, and no
/// space at either end. A row is written with no carriage return after it, so
/// a newline inside one puts every row after it in the wrong column, and on a
/// stream it gives a second line none of the wording that said what the first
/// one was.
#[test]
fn a_row_is_tamed_and_folded_onto_one_line() {
    assert_eq!(tame_line("one\ntwo"), "one two");
    assert_eq!(tame_line("one\n\n\ntwo"), "one two");
    assert_eq!(tame_line("  padded  "), "padded");
    assert_eq!(tame_line("a\u{1b}[2Jb\nc"), "ab c");
    assert_eq!(tame_line("\u{1b}]0;title\u{7}named"), "named");
    for text in ["a\nb", "\u{1b}[31mred\n", "one\r\ntwo"] {
        let row = tame_line(text);
        assert!(!row.contains('\n'), "{text:?}");
        assert!(!row.contains('\u{1b}'), "{text:?}");
    }
    // What was already one line is untouched.
    assert_eq!(tame_line("deepseek-chat"), "deepseek-chat");
}
