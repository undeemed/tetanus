//! Test Design Specification: what a hostile or truncated byte stream can do
//! to the transcript sanitizer.
//!
//! Feature under test: [`tetanus_exec::sanitize::Sanitizer`], fed bytes chosen
//! to break it rather than to print with it.
//!
//! Approach. `upstream_terminal_session.rs` TC-PORT-TERM-18 pins the one
//! split-sequence case upstream pins; this file asks what happens when the
//! bytes are wrong rather than merely divided. Every input is a shape a real
//! stream produces - a shell printing a 64 KiB `PS1`, a binary catted to a
//! terminal, a program killed mid-sequence, a `\r` landing on a read boundary.
//!
//! What the sanitizer must never do, in the order it matters: put an escape
//! byte in front of a model, grow without bound on input a child chooses, or
//! lose text that came after a sequence it gave up on.
//!
//! Environmental needs: none. No process is started and no descriptor opened.
//!
//! Pass criteria: each case's stated expected result exactly.
//! Fail criteria: any other value, or a panic.

use tetanus_exec::sanitize::{Sanitizer, PROMPT_MARKER_PREFIX};

/// TC-EXEC-SANE-1: an escape sequence a child never closes is given up on, and
/// the text after it still arrives.
///
/// This is the unbounded-buffer case. The sanitizer carries an unfinished
/// sequence between reads so that a split one is not half-printed, which means
/// a program that writes `ESC [` and then never writes a final byte is memory
/// the child is choosing the size of. The module bounds it; nothing ran the
/// bound.
///
/// Giving up is only half of it. Once the buffer is dropped, the *rest* of the
/// abandoned sequence is still coming, and its bytes are parameter digits and
/// then a final byte - all printable. A sanitizer that gave up without
/// remembering what it had given up on would print the tail of the sequence as
/// text, which is the failure this case names.
///
/// Input: a CSI introducer followed by more than the bound in parameter bytes,
/// split across reads, then the sequence's final byte, then real text.
/// Expected: no escape byte and no run of parameter digits is ever emitted,
/// and the text after the final byte arrives intact.
#[test]
fn a_sequence_a_child_never_closes_is_given_up_on_and_the_text_after_it_survives() {
    let mut sanitizer = Sanitizer::new();

    let opened = sanitizer.push("before\u{1b}[");
    assert_eq!(opened.text, "before", "text before the sequence is printed");

    // Past the bound. Chunked-versus-single is TC-EXEC-SANE-7's claim.
    let carried = sanitizer.push(&"1".repeat(9 * 1024));
    assert!(
        carried.text.is_empty(),
        "parameter bytes are not text: {:?}",
        carried.text
    );

    // The sequence finally ends. Its final byte belongs to the sequence and
    // must not be printed either.
    let closed = sanitizer.push("mafter");
    assert_eq!(
        closed.text, "after",
        "the tail of an abandoned sequence must not be printed as text"
    );
    assert_eq!(sanitizer.flush(), "", "nothing is left held");
}

/// TC-EXEC-SANE-2: the same for an OSC, which ends two different ways.
///
/// OSC carries a window title - arbitrary text the program chooses - so it is
/// the sequence most likely to be long, and it terminates with either BEL or
/// `ESC \`. A recovery that only knew about BEL would print the remainder of
/// every string-terminated title.
///
/// Input: an over-long OSC closed with BEL, then another closed with `ESC \`.
/// Expected: neither payload is printed, and the text after each arrives.
#[test]
fn an_over_long_osc_is_given_up_on_and_recovers_from_either_terminator() {
    for (name, terminator) in [("BEL", "\u{7}"), ("ST", "\u{1b}\\")] {
        let mut sanitizer = Sanitizer::new();
        let _ = sanitizer.push("\u{1b}]0;");
        let carried = sanitizer.push(&"t".repeat(9 * 1024));
        assert!(
            carried.text.is_empty(),
            "{name}: title bytes are not text: {:?}",
            carried.text
        );
        let closed = sanitizer.push(&format!("{terminator}visible"));
        assert_eq!(
            closed.text, "visible",
            "{name}: the text after the terminator arrives"
        );
    }
}

/// TC-EXEC-SANE-3: a lone escape byte at the end of a read is carried, not
/// printed, and is dropped if the stream ends there.
///
/// The escape byte is the one position where the sanitizer cannot yet know
/// what kind of sequence it has - the next byte decides - so it holds it. If
/// the child dies at that instant, the byte was never text and must not be
/// flushed as any.
///
/// Input: a chunk ending in a bare escape, then the rest of a CSI; and
/// separately, a chunk ending in a bare escape followed by a close.
/// Expected: nothing printed for the escape in either case, the completed
/// sequence consumed, and `flush` empty rather than emitting `\x1b`.
#[test]
fn a_lone_escape_at_the_end_of_a_read_is_carried_and_never_printed() {
    let mut resumed = Sanitizer::new();
    let held = resumed.push("text\u{1b}");
    assert_eq!(held.text, "text", "the escape byte is held back");
    let finished = resumed.push("[0mmore");
    assert_eq!(finished.text, "more", "the sequence completes and vanishes");

    let mut abandoned = Sanitizer::new();
    let _ = abandoned.push("text\u{1b}");
    assert_eq!(
        abandoned.flush(),
        "",
        "an escape the child never finished was never text"
    );
}

/// TC-EXEC-SANE-4: a carriage return that ends a read waits for the next one,
/// and is a newline if the stream ends instead.
///
/// A terminal writes `\r\n` for a new line and a bare `\r` to return to the
/// start of one, and a read can land between the two. Emitting the `\r` at
/// once turns one line break into two; holding it for ever loses the last line
/// of a program that ended on a bare `\r`.
///
/// The split-CRLF half is `upstream_terminal_session.rs` TC-PORT-TERM-18's
/// already; what is new here is the bare return and the stream that ends on
/// one.
///
/// Input: a chunk ending in `\r` followed by ordinary text, and a chunk ending
/// in `\r` followed by the stream closing.
/// Expected: a newline once something follows, and a newline from `flush`.
#[test]
fn a_carriage_return_on_a_read_boundary_is_one_newline_however_it_ends() {
    let mut bare = Sanitizer::new();
    assert_eq!(bare.push("a\r").text, "a");
    assert_eq!(
        bare.push("b").text,
        "\nb",
        "a bare return is a newline once something follows it"
    );

    let mut ended = Sanitizer::new();
    assert_eq!(ended.push("a\r").text, "a");
    assert_eq!(
        ended.flush(),
        "\n",
        "a stream that ended on a bare return still ended its line"
    );
}

/// TC-EXEC-SANE-5: a prompt marker whose status is not a number is reported as
/// a marker with no status, and never as text.
///
/// The status comes from the shell, and a shell that has been told something
/// odd - or a program forging the sequence - produces a marker whose payload
/// is not an integer. Dropping the marker would leave the session waiting for
/// a prompt that already happened; printing it would put `133;D;` in front of
/// a model.
///
/// Input: markers carrying a negative status, a status padded with spaces, an
/// empty status, and a number too large for `i32`. The ordinary statuses are
/// TC-EXEC-SANE-6's.
/// Expected: every one is a marker; the parseable ones carry their value, the
/// rest carry none; none of them appears in the text.
#[test]
fn a_prompt_marker_with_an_unparseable_status_is_still_a_marker() {
    let cases: [(&str, Option<i32>); 4] = [
        ("-1", Some(-1)),
        (" 7 ", Some(7)),
        ("", None),
        ("99999999999999999999", None),
    ];
    for (status, expected) in cases {
        let mut sanitizer = Sanitizer::new();
        let said = sanitizer.push(&format!("\u{1b}]{PROMPT_MARKER_PREFIX}{status}\u{7}x"));
        assert_eq!(
            said.prompts,
            vec![expected],
            "`{status}` is a marker carrying {expected:?}"
        );
        assert_eq!(said.text, "x", "`{status}`: the marker is not text");
    }
}

/// TC-EXEC-SANE-6: several commands finishing between two reads are several
/// markers, in order.
///
/// A burst - a shell running a pipeline, or output arriving after a stall -
/// puts more than one marker in a chunk. A sanitizer that reported only the
/// last would make a session think one command finished when three did, and
/// attribute the wrong exit status to the one it noticed.
///
/// Input: three markers with distinct statuses in one chunk, with text between
/// them.
/// Expected: three statuses in the order they were printed, and only the text.
#[test]
fn several_markers_in_one_read_are_all_reported_in_order() {
    let mut sanitizer = Sanitizer::new();
    let said = sanitizer.push(&format!(
        "a\u{1b}]{PROMPT_MARKER_PREFIX}1\u{7}b\u{1b}]{PROMPT_MARKER_PREFIX}0\u{7}c\
         \u{1b}]{PROMPT_MARKER_PREFIX}130\u{7}d"
    ));
    assert_eq!(said.prompts, vec![Some(1), Some(0), Some(130)]);
    assert_eq!(said.text, "abcd");
}

/// TC-EXEC-SANE-7: bytes that are not a sequence at all are printed, and the
/// ones that would confuse a reader are not.
///
/// Between "this is a control sequence" and "this is text" sits a set of bytes
/// that is neither: a NUL, a BEL outside any sequence, a two-byte escape, and
/// a multi-byte character immediately after an escape. The last is the one
/// that can go wrong quietly - the two-byte escape path advances by the
/// character's UTF-8 length, and advancing by one byte instead would split a
/// character and produce invalid text.
///
/// Input: a two-byte escape, an escape followed by a multi-byte character, a
/// stray BEL, and a NUL.
/// Expected: no escape byte survives, the multi-byte character after an escape
/// is consumed as part of it rather than split, and a NUL is passed through as
/// the ordinary character it is.
#[test]
fn bytes_that_are_not_a_sequence_are_handled_without_corrupting_the_text() {
    let mut sanitizer = Sanitizer::new();

    // `ESC 7` is save-cursor: two bytes, both gone.
    assert_eq!(sanitizer.push("a\u{1b}7b").text, "ab");

    // An escape followed by a character that is three bytes in UTF-8. Both the
    // escape and the whole character belong to the sequence; taking one byte
    // of it would leave two continuation bytes behind.
    let multibyte = sanitizer.push("c\u{1b}\u{4e16}d");
    assert_eq!(
        multibyte.text, "cd",
        "a multi-byte escape is consumed whole"
    );

    // A BEL outside any sequence is a bell, not text.
    assert_eq!(sanitizer.push("e\u{7}f").text, "ef");

    // A NUL is not a control sequence and not a bell, so it is text like any
    // other character. Asserting it survives pins that the escape scanner does
    // not treat an arbitrary control byte as the start of something.
    assert_eq!(sanitizer.push("g\u{0}h").text, "g\u{0}h");
}
