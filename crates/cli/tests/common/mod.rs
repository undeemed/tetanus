//! Helpers shared by the end-to-end cases.
//!
//! One copy rather than one per file: the two readers below both compare two
//! runs of the same binary for equality, so both have to agree on which part
//! of a run a repeated run is not required to repeat.

/// The page with the turn's wall clock taken out.
///
/// `render::timeline` prints how long a turn took only once it passes a
/// second, on the reasoning that an offline turn never does. A loaded runner
/// disproves that: one of two otherwise identical runs crosses the second and
/// the other does not, and a byte-for-byte comparison then fails on the one
/// field neither case is about. TC-CLI-2 asserts reproducibility and
/// TC-CLI-UI-4 asserts colour policy, so the field is dropped from both sides
/// before they are compared. The field itself stays asserted by TC-CLI-TL-13,
/// against a journal whose timestamps are fixed.
pub fn without_duration(page: &str) -> String {
    page.split_inclusive('\n').map(drop_duration).collect()
}

/// One line, minus its wall clock if it carries one.
fn drop_duration(line: &str) -> String {
    let (body, end) = match line.strip_suffix('\n') {
        Some(body) => (body, "\n"),
        None => (line, ""),
    };
    // The closing line of a turn is the only line built out of separated
    // segments, and the only one that ever carries a duration.
    if !body.starts_with("turn ") {
        return line.to_string();
    }
    let dot = if body.contains(" \u{b7} ") {
        " \u{b7} "
    } else {
        " - "
    };
    let kept: Vec<&str> = body.split(dot).filter(|part| !is_duration(part)).collect();
    format!("{}{end}", kept.join(dot))
}

/// A segment that reads as a wall clock: `1.1s`, `2m05s`. `2 steps` ends in
/// `s` as well, so a segment carrying a space is never one.
fn is_duration(part: &str) -> bool {
    !part.contains(' ') && part.ends_with('s') && part.starts_with(|c: char| c.is_ascii_digit())
}

/// TC-CLI-COMMON-1: the helper drops the wall clock and nothing else.
/// Expected: a closing line loses `1.1s` in either charset and keeps its turn
/// number, reason, step count and token tally; a closing line without one is
/// returned unchanged; and no other line is touched. A helper that quietly
/// stripped nothing would restore the flake it exists to remove, and one that
/// stripped too much would hollow out the two cases that use it.
///
/// Environmental needs: none. This case runs in both test binaries that
/// include the module, which is intended - each gets the check.
#[test]
fn the_wall_clock_is_the_only_thing_dropped() {
    let cases = [
        (
            "answer\n\nturn 1 \u{b7} natural \u{b7} 2 steps \u{b7} 1.1s \u{b7} 65 tokens\n",
            "answer\n\nturn 1 \u{b7} natural \u{b7} 2 steps \u{b7} 65 tokens\n",
        ),
        (
            "turn 1 - natural - 2 steps - 1m05s - 65 tokens\n",
            "turn 1 - natural - 2 steps - 65 tokens\n",
        ),
        (
            "turn 1 \u{b7} natural \u{b7} 1 step \u{b7} 12 tokens\n",
            "turn 1 \u{b7} natural \u{b7} 1 step \u{b7} 12 tokens\n",
        ),
        ("turns take 2.5s to read\n", "turns take 2.5s to read\n"),
    ];
    for (page, want) in cases {
        assert_eq!(without_duration(page), want);
    }
}
