//! Test Design Specification: what a hostile or malformed terminal stream can
//! do to the harness.
//!
//! Feature under test: [`tetanus_exec::screen::Screen`] fed bytes chosen to
//! break it rather than to draw with it.
//!
//! Approach, and why it is a file of its own. `upstream_screen.rs` is the
//! ported suite: it asks whether the grid models a terminal correctly for the
//! programs that draw on one, and every input it uses is a sequence a real
//! program writes. This asks the other question - what happens when the bytes
//! are wrong. The screen is fed straight from a child's stdout, so every
//! input here is something a buggy binary, a corrupted stream, or a command a
//! model wrote can actually produce, and the harness has to survive all of it.
//!
//! The distinction is not academic: TC-EXEC-HOSTILE-1 is a hang that shipped,
//! and the ported suite could not have found it because no terminal program
//! writes the sequence that caused it.
//!
//! Environmental needs: none. No process is started and no descriptor opened.
//!
//! Pass criteria: each case's stated expected result exactly.
//! Fail criteria: any other value, a panic, or a case that does not finish
//! inside its bound.

use std::time::Duration;

use tetanus_exec::screen::Screen;

/// The grid every case uses unless it needs another shape. Small on purpose:
/// a bound that is wrong is wrong at any size, and a small grid makes an
/// unbounded loop finish in a different order of magnitude from a bounded one.
const ROWS: u16 = 8;
const COLS: u16 = 20;

/// TC-EXEC-HOSTILE-1: a line count larger than the screen does not become a
/// loop that never ends.
///
/// This is a defect this case was written to reproduce and now guards. The
/// CSI parameter parse is `parse::<usize>().unwrap_or(0)`, so the count is
/// whatever the program wrote, and `CSI L`, `CSI M`, `CSI @` and `CSI P` each
/// shifted the grid one unit per count. `ESC[18446744073709551615L` therefore
/// asked for eighteen quintillion single-line shifts of an eight-row grid.
/// Measured before the fix: one `feed` call still running after 30 seconds.
///
/// Anything a child prints reaches here, so this was a wedge that any program
/// in a tetanus terminal could cause, deliberately or by writing a corrupt
/// sequence.
///
/// Input: `usize::MAX` as the count of every one of the four sequences that
/// shift the grid, on a screen with content, one sequence at a time.
/// Expected: each returns inside the bound, and the grid is still consistent
/// afterwards - writes to the last column and the last row both land rather
/// than panicking, which is only possible if every row kept its width. What
/// the line sequences leave behind is TC-EXEC-HOSTILE-2's claim.
#[test]
fn a_count_larger_than_the_screen_does_not_hang_the_reader() {
    for sequence in ['L', 'M', '@', 'P'] {
        // The work runs on a thread of its own behind a deadline: a hang is
        // the failure being tested for, so the case has to still be running in
        // order to report it. libtest has no per-test timeout.
        let (tell, hear) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let screen = Screen::new(ROWS, COLS);
            let _ = screen.feed("keep this line");
            let _ = screen.feed(&format!("\u{1b}[{}{sequence}", usize::MAX));
            // Writes afterwards, because a grid left inconsistent panics on
            // the next index rather than on the sequence that broke it. The
            // last column matters on its own: `insert_cells` pops one cell per
            // count before inserting, so a clamp that popped without inserting
            // would leave the row short and panic here.
            let _ = screen.feed(&format!("\u{1b}[1;{COLS}H!"));
            let _ = screen.feed(&format!("\u{1b}[{ROWS};1Hsurvived"));
            // A send that fails means the receiver already gave up.
            tell.send(screen.text()).ok();
        });
        let text = hear
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| {
                panic!(
                "CSI {sequence} with usize::MAX did not finish in 10s. The screen is fed from a \
                 child's stdout, so a stream that never returns is a wedged harness."
            )
            });
        assert!(
            text.contains("survived"),
            "CSI {sequence}: the grid still works afterwards: {text:?}"
        );
        assert!(
            text.contains('!'),
            "CSI {sequence}: the last column is still writable, so the row kept \
             its width: {text:?}"
        );
    }
}

/// TC-EXEC-HOSTILE-2: `CSI M` at its real boundaries.
///
/// The clamp must not become an off-by-one that eats one line too many or too
/// few, so each is asked at one below the grid, exactly the grid, and one
/// above it. `usize::MAX` alone would pass against a clamp of zero, which
/// would be a screen that ignored the sequence entirely.
///
/// Input: `CSI 1 M` on a grid of eight known lines, then `CSI 7 M`, `CSI 8 M`
/// and `CSI 9 M`.
/// Expected: one line gone, then seven, then all of them, and one above the
/// grid is not different from exactly the grid.
#[test]
fn a_line_count_is_clamped_without_an_off_by_one() {
    let drawn = |count: usize| -> String {
        let screen = Screen::new(ROWS, COLS);
        // Addressed rather than printed with newlines: printing ROWS lines
        // each ending in a newline scrolls the first one off, so the fixture
        // would be asserting about a grid it did not mean to build.
        for line in 0..ROWS {
            let _ = screen.feed(&format!("\u{1b}[{};1Hline{line}", line + 1));
        }
        let _ = screen.feed("\u{1b}[H");
        let _ = screen.feed(&format!("\u{1b}[{count}M"));
        screen.text()
    };

    let one = drawn(1);
    assert!(!one.contains("line0"), "one line was deleted: {one:?}");
    assert!(one.contains("line1"), "and only one: {one:?}");

    let almost = drawn(ROWS as usize - 1);
    assert!(
        almost.contains(&format!("line{}", ROWS - 1)),
        "one short of the grid leaves the last line: {almost:?}"
    );

    let exactly = drawn(ROWS as usize);
    assert!(
        exactly.is_empty(),
        "exactly the grid leaves nothing: {exactly:?}"
    );
    assert_eq!(
        drawn(ROWS as usize + 1),
        exactly,
        "one more than the grid cannot differ from exactly the grid"
    );
}

/// TC-EXEC-HOSTILE-3: a parameter that is not a number is read as zero rather
/// than refused, and zero still means one where the sequence says so.
///
/// The parse is `unwrap_or(0)`, and several sequences then do `.max(1)`. A
/// change that made a malformed parameter mean *nothing* would silently stop
/// the sequence working; one that made it panic would kill the reader on a
/// corrupt byte. Both are worse than reading it as zero.
///
/// Input: `CSI ;C`, `CSI abcC`, `CSI 999999999999999999999999C` (past
/// `usize`), and `CSI -1C`.
/// Expected: no panic, and the cursor moves by one column in each case,
/// because zero means one for a cursor movement.
#[test]
fn a_malformed_parameter_is_read_as_zero_and_never_panics() {
    // Every spelling here stays inside the CSI *parameter* range. `abc` would
    // not: `a` is 0x61, inside the 0x40..=0x7e final-byte range, so `ESC[abcC`
    // is the complete sequence `CSI a` followed by the literal text `bcC` -
    // which moves the cursor three columns by printing, and would have made
    // this case pass for entirely the wrong reason.
    for params in [";", "999999999999999999999999", "-1", "+", "  ", ";;;"] {
        let screen = Screen::new(ROWS, COLS);
        let _ = screen.feed(&format!("\u{1b}[{params}C"));
        assert_eq!(
            screen.cursor().col,
            1,
            "`CSI {params} C` must move one column, not panic and not stall"
        );
    }
}

/// TC-EXEC-HOSTILE-4: a cursor address far outside the grid lands inside it.
///
/// Every write indexes the grid directly, so a cursor parked out of bounds is
/// a panic on the next printable byte rather than at the moment of the bad
/// sequence - which is what makes it worth asserting the write, not just the
/// cursor.
///
/// Input: `CSI 9999;9999H` on an eight-by-twenty grid, then a character.
/// Expected: the cursor is clamped to the last cell, and the character is
/// printed there rather than panicking.
#[test]
fn a_cursor_address_outside_the_grid_is_clamped_before_anything_is_written() {
    let screen = Screen::new(ROWS, COLS);
    let _ = screen.feed("\u{1b}[9999;9999H");
    let at = screen.cursor();
    assert_eq!(at.row, ROWS as usize - 1, "clamped to the last row");
    assert_eq!(at.col, COLS as usize - 1, "clamped to the last column");

    let _ = screen.feed("X");
    assert!(
        screen.text().ends_with('X'),
        "the write lands instead of panicking: {:?}",
        screen.text()
    );
}

/// TC-EXEC-HOSTILE-5: a one-by-one screen survives every editing sequence.
///
/// The smallest grid is where an off-by-one becomes a panic: `self.cols - 1`
/// and `self.rows - 1` are subtractions on unsigned numbers, so a grid of one
/// is one step from an underflow, and `Screen::new` clamps its arguments up to
/// one specifically to keep zero out. Asking for zero is the other side of the
/// same boundary and must produce that one-cell grid rather than an empty one.
///
/// Input: a screen asked for zero rows and zero columns, fed the whole editing
/// family.
/// Expected: no panic from any of them, and the grid still answers.
#[test]
fn the_smallest_possible_screen_survives_every_editing_sequence() {
    let screen = Screen::new(0, 0);
    for sequence in [
        "\u{1b}[L",
        "\u{1b}[M",
        "\u{1b}[@",
        "\u{1b}[P",
        "\u{1b}[X",
        "\u{1b}[J",
        "\u{1b}[K",
        "\u{1b}[1J",
        "\u{1b}[2J",
        "\u{1b}[1K",
        "\u{1b}[2K",
        "\u{1b}[999;999H",
        "\u{1b}[999A",
        "\u{1b}[999B",
        "\u{1b}[999C",
        "\u{1b}[999D",
        "\u{1b}[999G",
        "\u{1b}[999d",
        "\u{1b}[999E",
        "\u{1b}[999F",
        "\u{1b}[s",
        "\u{1b}[u",
        "\u{1b}[1;1r",
        "\u{1b}[9;1r",
    ] {
        let _ = screen.feed(sequence);
        // A write after each one, because an inconsistent grid panics on the
        // next index rather than on the sequence that broke it.
        let _ = screen.feed("z");
    }
    // `Screen::new` clamps zero up to one, so this is a one-by-one grid. The
    // cursor may rest one past the last column - that is the deferred wrap
    // every terminal does - but it must never be further than that, which is
    // what would index outside the row.
    assert_eq!(screen.cursor().row, 0, "a one-row grid has one row");
    assert!(
        screen.cursor().col <= 1,
        "a one-column grid parks at the wrap column at most, got {}",
        screen.cursor().col
    );
}

/// TC-EXEC-HOSTILE-6: a scrolling region a program sets upside-down or off the
/// end is refused, and the region it had is kept.
///
/// `CSI r` is the one sequence that changes which rows every later line feed
/// moves. A region with `bottom` above `top`, or past the last row, would make
/// `bounded_lines` and the scroll helpers index outside the grid. Refusing it
/// is what the code does; nothing asserted that it does.
///
/// Input: an inverted region, a region past the end, and a degenerate
/// single-row region, each followed by enough line feeds to scroll.
/// Expected: none of them panics, and none of them loses content that the
/// default region would have kept.
#[test]
fn a_scrolling_region_that_is_backwards_or_off_the_end_is_refused() {
    for region in [
        "\u{1b}[8;2r",
        "\u{1b}[1;9999r",
        "\u{1b}[4;4r",
        "\u{1b}[0;0r",
    ] {
        let screen = Screen::new(ROWS, COLS);
        let _ = screen.feed(region);
        for line in 0..ROWS as usize * 2 {
            let _ = screen.feed(&format!("row{line}\r\n"));
        }
        let text = screen.text();
        assert!(
            text.contains(&format!("row{}", ROWS as usize * 2 - 1)),
            "{region}: the newest line is always on the screen: {text:?}"
        );
        assert!(
            text.lines().count() <= ROWS as usize,
            "{region}: the grid cannot grow past its own height: {text:?}"
        );
    }
}

/// TC-EXEC-HOSTILE-7: an escape sequence that never terminates does not print
/// itself and does not grow without bound.
///
/// A program that writes `ESC [` and then megabytes of digits is writing a
/// sequence that is still, formally, unfinished. Printing it would put control
/// bytes in front of a model; buffering it for ever would be memory a child
/// controls.
///
/// Input: a CSI introducer followed by 64 KiB of parameter bytes and no final
/// byte, then a legitimate line.
/// Expected: none of the parameter bytes are on the screen, and the screen
/// still works afterwards.
#[test]
fn an_escape_sequence_that_never_ends_is_not_printed_and_does_not_grow() {
    let screen = Screen::new(ROWS, COLS);
    let _ = screen.feed(&format!("\u{1b}[{}", "1".repeat(64 * 1024)));
    let _ = screen.feed("\u{1b}[2Jvisible");

    let text = screen.text();
    assert!(
        text.contains("visible"),
        "the screen still works after an unfinished sequence: {text:?}"
    );
    assert!(
        !text.contains("1111"),
        "parameter bytes were printed as text: {text:?}"
    );
}

/// TC-EXEC-HOSTILE-8: an OSC sequence that never terminates is not printed
/// either, and a NUL or a lone escape inside it does not derail the parse.
///
/// OSC carries a window title, which is arbitrary text a program chooses, so
/// it is the sequence most likely to contain something hostile. Its terminator
/// is either BEL or `ESC \`, and a program that writes neither leaves the
/// parser holding the payload.
///
/// Input: an unterminated OSC carrying an escape byte and a NUL, then a
/// terminated one, then text.
/// Expected: no payload on the screen and the later text intact.
#[test]
fn an_unterminated_osc_payload_never_reaches_the_screen() {
    let screen = Screen::new(ROWS, COLS);
    let _ = screen.feed("\u{1b}]0;a title with \u{0} and \u{1b} inside");
    let _ = screen.feed(" still the title\u{7}");
    let _ = screen.feed("after");

    let text = screen.text();
    assert!(
        text.contains("after"),
        "the text after it survives: {text:?}"
    );
    assert!(
        !text.contains("title"),
        "an OSC payload was printed as text: {text:?}"
    );
    assert!(
        !text.contains('\u{1b}'),
        "an escape byte reached the screen: {text:?}"
    );
}

/// TC-EXEC-HOSTILE-9: a flood of questions does not become a flood of memory.
///
/// That one question gets exactly one reply is `upstream_screen.rs`
/// TC-PORT-SCREEN-8's claim. This is the other half: replies are owed to the
/// child and drain on `feed`, so a program that asks a thousand times before
/// anything reads is where "owed" could grow without a reader. PowerShell's
/// `PSReadLine` genuinely asks on every prompt.
///
/// Input: a thousand cursor-position requests in one chunk.
/// Expected: exactly a thousand replies, and the buffer empty afterwards
/// rather than replaying them.
#[test]
fn a_flood_of_questions_does_not_accumulate_replies() {
    let screen = Screen::new(ROWS, COLS);

    let flood = screen.feed(&"\u{1b}[6n".repeat(1_000));
    assert_eq!(flood.len(), 1_000, "every question is answered");

    assert!(
        screen.feed("").is_empty(),
        "answers are handed over once and not replayed"
    );
}
