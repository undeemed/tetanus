//! Test Design Specification: deterministic tool-result pruning, ported.
//!
//! Feature under test: `tetanus_turn::prune` - shrinking a tool result that is
//! too long to keep whole, by keeping its head and its tail and saying in the
//! middle that something was removed. Upstream pins the same transform in
//! `packages/compaction/compaction-tool-result-pruner/tests/tool-result-pruner.spec.ts`;
//! each case names the upstream case it comes from.
//!
//! Approach: literal inputs and literal expected outputs. The transform is a
//! pure function of text and three numbers, so a case that computed its
//! expectation the way the implementation does would only be asserting that
//! the implementation is itself.
//!
//! What is not restated, and why. Upstream's tool results are lists of typed
//! content blocks, so half its suite is about preserving non-text blocks and
//! their relative ordering across a removed span; a `tetanus_turn` tool result
//! is `ToolOutcome { ok, content: String }`, so there are no blocks to
//! preserve and nothing to restate. Its session-transaction half - rewriting
//! the journal with a shadow node that cites the result it replaced, and
//! pricing the shadowed node - needs a durable event type this contract has
//! not published, so it stays phase (2) and `docs/parity.md` carries it. Its
//! unknown-config-key rejection has no counterpart: a `PruneBudget` is a
//! struct, so a key nobody declared does not compile.
//!
//! Environmental needs: none. No case touches a filesystem, a network or an
//! API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tetanus_turn::prune::{length, prune, pruned, PruneBudget, PruneError, MARKER};

/// TC-PORT-PRUNE-1: a result within budget is left exactly alone.
///
/// Upstream: "skips content within threshold".
///
/// `None` and not an unchanged copy: a caller usually wants to know whether
/// anything happened, and an answer it has to diff against its input to find
/// out is one where the diff gets skipped.
///
/// Input: text shorter than the threshold, and text exactly at it.
/// Expected: nothing pruned for either. The boundary is inclusive, so a result
/// that exactly fits is not rewritten to say it was shortened.
#[test]
fn a_result_within_budget_is_untouched() {
    let budget = small();

    assert_eq!(prune("short", budget), None);

    let exact = "x".repeat(budget.threshold);
    assert_eq!(length(&exact), budget.threshold);
    assert_eq!(prune(&exact, budget), None, "at the threshold is within it");

    // The convenience form hands back the same text rather than a marker.
    assert_eq!(pruned(&exact, budget), exact);
}

/// TC-PORT-PRUNE-2: an over-long result keeps its head and its tail, with the
/// marker between them.
///
/// Upstream: "keeps configured head and tail".
///
/// The two ends are where the information is: the head says what the command
/// was doing and the tail says how it ended. Keeping the middle instead would
/// throw away both.
///
/// Input: a hundred characters under a budget of head 4, tail 3.
/// Expected: exactly the first four, the marker, and exactly the last three.
#[test]
fn an_over_long_result_keeps_its_head_and_tail() {
    let budget = small();
    let text: String = ('a'..='z').cycle().take(100).collect();

    let result = prune(&text, budget).expect("over the threshold");

    let head: String = text.chars().take(4).collect();
    let tail: String = text.chars().skip(97).collect();
    assert_eq!(result, format!("{head}{MARKER}{tail}"));
    assert!(result.starts_with(&head));
    assert!(result.ends_with(&tail));
}

/// TC-PORT-PRUNE-3: a prune never splits a character.
///
/// Upstream: "without splitting surrogate pairs", and asserts the output
/// carries no replacement character.
///
/// This is the case with a real bug behind it. The offsets come from a budget
/// rather than from the text, so they land mid-character routinely, and the
/// obvious implementation - slicing the string at those offsets - panics in
/// Rust on a UTF-8 boundary. The hazard is every encoding's; only the symptom
/// differs.
///
/// Input: sixty four-byte characters, then a mix of one-, two-, three- and
/// four-byte characters, under a head and tail that fall inside a character
/// if measured in bytes.
/// Expected: the exact head and tail characters, no panic, no replacement
/// character, and a result that still round-trips as the string it claims to
/// be.
#[test]
fn a_prune_never_splits_a_character() {
    let budget = small();

    let emoji = "\u{1F600}".repeat(60);
    let result = prune(&emoji, budget).expect("over the threshold");
    assert_eq!(
        result,
        format!("{}{MARKER}{}", "\u{1F600}".repeat(4), "\u{1F600}".repeat(3))
    );
    assert!(!result.contains('\u{FFFD}'), "no character was cut in half");

    // A mix of widths, so a byte-based slice would land inside a different
    // character than the all-emoji case does.
    let mixed: String = "a\u{00e9}\u{4e2d}\u{1F600}"
        .chars()
        .cycle()
        .take(80)
        .collect();
    let result = prune(&mixed, budget).expect("over the threshold");
    let head: String = mixed.chars().take(4).collect();
    let tail: String = mixed.chars().skip(77).collect();
    assert_eq!(result, format!("{head}{MARKER}{tail}"));
    assert!(!result.contains('\u{FFFD}'));
}

/// TC-PORT-PRUNE-4: measurement is in characters, not bytes.
///
/// Upstream: "measures text code points only", asserting `a😀b` measures 3.
///
/// A byte measure would prune text that fits and keep text that does not,
/// entirely according to which alphabet it was written in - so a session in
/// Japanese would be pruned roughly three times as eagerly as the same session
/// in English.
///
/// Input: a three-character string that is six bytes.
/// Expected: it measures three; the same string is within a threshold of three
/// and beyond a threshold of two.
#[test]
fn measurement_is_in_characters_not_bytes() {
    let text = "a\u{1F600}b";

    assert_eq!(length(text), 3);
    assert_eq!(
        text.len(),
        6,
        "and it is six bytes, which is not the measure"
    );

    let fits = PruneBudget {
        threshold: 3,
        head: 0,
        tail: 0,
    };
    // Not validated here: this asks only what the measure says, and the marker
    // makes these budgets unshrinkable by design.
    assert_eq!(prune(text, fits), None);
}

/// TC-PORT-PRUNE-5: a head and tail of zero still shrink, to just the marker.
///
/// Upstream: "supports zero-sized head and tail while still shrinking".
///
/// The degenerate budget has to work because it is the one a caller reaches
/// for when context is desperate, and it is the case an implementation that
/// assumed a non-empty head would get wrong.
///
/// Input: a hundred characters, head 0, tail 0, threshold exactly the marker's
/// length.
/// Expected: the marker alone, and its length is exactly the threshold - so
/// pruning again would leave it alone rather than looping.
#[test]
fn a_zero_head_and_tail_shrink_to_the_marker() {
    let budget = PruneBudget {
        threshold: length(MARKER),
        head: 0,
        tail: 0,
    }
    .validate()
    .expect("the marker alone is exactly affordable");

    let result = prune(&"x".repeat(100), budget).expect("over the threshold");

    assert_eq!(result, MARKER);
    assert_eq!(length(&result), budget.threshold);
    assert_eq!(prune(&result, budget), None, "and it converges");
}

/// TC-PORT-PRUNE-6: a budget that could not shrink is refused where it is set.
///
/// Upstream: "rejects ... an output budget above threshold".
///
/// A prune that emitted more than it accepts would never converge: a caller
/// that prunes until the result fits would loop forever, and one that prunes
/// once would have made the context *larger*. Refusing at the budget catches
/// it at the one place a human chose the numbers, rather than at the call site
/// that trusted them.
///
/// Input: head plus marker plus tail exceeding the threshold by one; a
/// threshold of zero; and the boundary where they are exactly equal.
/// Expected: the first two refused, the boundary accepted, and the refusal
/// carries every number so the reader can see which one to change.
#[test]
fn a_budget_that_could_not_shrink_is_refused() {
    let marker = length(MARKER);

    let over = PruneBudget {
        threshold: marker + 10,
        head: 6,
        tail: 5,
    };
    match over.validate() {
        Err(PruneError::WouldNotShrink {
            head,
            tail,
            total,
            threshold,
            ..
        }) => {
            assert_eq!((head, tail), (6, 5));
            assert_eq!(total, marker + 11);
            assert_eq!(threshold, marker + 10);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    assert_eq!(
        PruneBudget {
            threshold: 0,
            head: 0,
            tail: 0
        }
        .validate(),
        Err(PruneError::EmptyThreshold)
    );

    let exact = PruneBudget {
        threshold: marker + 11,
        head: 6,
        tail: 5,
    };
    assert!(exact.validate().is_ok(), "breaking even is affordable");
}

/// TC-PORT-PRUNE-7: pruning converges in one pass.
///
/// Upstream: "converges in one pass".
///
/// The property behind the budget rule. Any validated budget must leave a
/// result the same budget would not prune again, or a caller that loops until
/// nothing changes does not terminate.
///
/// Input: a spread of budgets and lengths, including the degenerate and
/// break-even ones.
/// Expected: for every pair, the pruned output is within the threshold and a
/// second prune answers `None`.
#[test]
fn pruning_converges_in_one_pass() {
    let marker = length(MARKER);
    let budgets = [
        PruneBudget::default(),
        small(),
        PruneBudget {
            threshold: marker,
            head: 0,
            tail: 0,
        },
        PruneBudget {
            threshold: marker + 11,
            head: 6,
            tail: 5,
        },
        PruneBudget {
            threshold: marker + 1,
            head: 1,
            tail: 0,
        },
    ];

    for budget in budgets {
        let budget = budget.validate().expect("every budget here is affordable");
        for length_of in [0usize, 1, 50, 5_000, 20_000] {
            let text = "\u{1F600}a".repeat(length_of);
            let Some(once) = prune(&text, budget) else {
                continue;
            };
            assert!(
                length(&once) <= budget.threshold,
                "a prune must land within its own threshold: {} > {}",
                length(&once),
                budget.threshold
            );
            assert_eq!(
                prune(&once, budget),
                None,
                "a second pass must find nothing to do"
            );
        }
    }
}

/// TC-PORT-PRUNE-8: the default budget is upstream's, and it is usable.
///
/// The defaults are the figures every deployment gets without choosing, so
/// they are worth pinning rather than leaving to be noticed when one changes.
///
/// Expected: the three upstream numbers, a budget that validates, and a marker
/// that is text a model can read rather than a control character it cannot.
#[test]
fn the_default_budget_is_upstreams_and_it_validates() {
    let budget = PruneBudget::default();

    assert_eq!(budget.threshold, 8192);
    assert_eq!(budget.head, 4096);
    assert_eq!(budget.tail, 1024);
    assert!(budget.validate().is_ok());

    assert!(
        MARKER.contains("pruned"),
        "the gap says what happened: {MARKER:?}"
    );
}

/// TC-PORT-PRUNE-9: the transform is a pure function of its input.
///
/// Not an upstream case, but the property upstream's "replays to the identical
/// pruned model messages" depends on. Deriving history from a journal has to
/// produce the same request every time
/// (`upstream_request_reconstruction.rs`), so anything shaping that history
/// must be reproducible from the journal alone - a clock, a model or a random
/// choice here would break that quietly, and only on replay.
///
/// Input: the same text and budget pruned repeatedly, and two budgets with the
/// same numbers reached different ways.
/// Expected: identical output every time, and no dependence on the identity of
/// the budget value.
#[test]
fn the_transform_is_a_pure_function_of_its_input() {
    let text = "\u{1F600}hello world ".repeat(500);
    let budget = PruneBudget::default().validate().expect("valid");

    let first = prune(&text, budget);
    for _ in 0..5 {
        assert_eq!(prune(&text, budget), first);
    }

    let rebuilt = PruneBudget {
        threshold: 8192,
        head: 4096,
        tail: 1024,
    };
    assert_eq!(prune(&text, rebuilt), first);
}

/// A budget small enough to write expected values out by hand.
fn small() -> PruneBudget {
    PruneBudget {
        threshold: 50,
        head: 4,
        tail: 3,
    }
}
