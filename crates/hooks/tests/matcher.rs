//! Conformance: which configured hooks an event selects.
//!
//! Feature under test: `tetanus_hooks::matcher` — the pattern beside each
//! configured hook, and the two dialects' different readings of it.
//!
//! Ported from upstream `packages/hooks/hook-protocol/tests/matcher.spec.ts`.
//! Case ids TC-HOOK-MATCH-1..9. The last two are this port's own: upstream
//! cannot ask them because JavaScript's regex engine answers them differently.

use tetanus_hooks::{matcher_diagnostic, matches_matcher, MatcherMode};

use MatcherMode::{ClaudeCode, Codex};

/// TC-HOOK-MATCH-1: absent, empty and `*` select everything, in both dialects.
#[test]
fn the_match_all_sentinels_select_everything_in_both_dialects() {
    for mode in [ClaudeCode, Codex] {
        assert!(matches_matcher(None, "Bash", mode), "absent, {mode:?}");
        assert!(
            matches_matcher(Some(""), "anything", mode),
            "empty, {mode:?}"
        );
        assert!(
            matches_matcher(Some("*"), "whatever", mode),
            "star, {mode:?}"
        );
    }
}

/// TC-HOOK-MATCH-2: a Claude word pattern is a literal *exact* match.
///
/// This is the case that makes the dialect split matter: read as a regex,
/// `Bash` would select `BashOutput` too, and a hook meant for one tool would
/// fire for another.
#[test]
fn a_claude_word_pattern_is_an_exact_match_not_a_substring() {
    assert!(matches_matcher(Some("Bash"), "Bash", ClaudeCode));
    assert!(!matches_matcher(Some("Bash"), "BashOutput", ClaudeCode));
}

/// TC-HOOK-MATCH-3: a Claude pipe pattern is literal alternation, and each
/// alternative is still exact.
#[test]
fn a_claude_pipe_pattern_is_literal_alternation() {
    assert!(matches_matcher(Some("Edit|Write"), "Edit", ClaudeCode));
    assert!(matches_matcher(Some("Edit|Write"), "Write", ClaudeCode));
    assert!(!matches_matcher(Some("Edit|Write"), "Read", ClaudeCode));
    assert!(!matches_matcher(Some("Edit|Write"), "EditFile", ClaudeCode));
}

/// TC-HOOK-MATCH-4: a Claude pattern outside the literal charset falls through
/// to an unanchored regex.
#[test]
fn a_claude_pattern_with_regex_syntax_is_a_regex() {
    assert!(matches_matcher(Some("^Bash$"), "Bash", ClaudeCode));
    assert!(matches_matcher(Some("Bash.*"), "BashOutput", ClaudeCode));
    assert!(matches_matcher(Some(r".*\.ts$"), "foo.ts", ClaudeCode));
    assert!(!matches_matcher(Some(r".*\.ts$"), "foo.js", ClaudeCode));
}

/// TC-HOOK-MATCH-5: Codex has no literal path, so a word pattern is a regex
/// and matches a substring — the opposite of TC-HOOK-MATCH-2 on the same input.
#[test]
fn a_codex_word_pattern_is_an_unanchored_regex() {
    assert!(matches_matcher(Some("Bash"), "Bash", Codex));
    assert!(matches_matcher(Some("Bash"), "BashOutput", Codex));
}

/// TC-HOOK-MATCH-6: alternation and anchors mean what a regex means, in Codex.
#[test]
fn codex_alternation_and_anchors_are_regex_operators() {
    assert!(matches_matcher(Some("Edit|Write"), "Edit", Codex));
    assert!(matches_matcher(Some("^Bash$"), "Bash", Codex));
    assert!(!matches_matcher(Some("^Bash$"), "BashOutput", Codex));
}

/// TC-HOOK-MATCH-7: an uncompilable pattern selects nothing, and does not
/// panic. Matching runs inside a turn, so the failure mode has to be "this
/// hook does not fire", never "this turn dies".
#[test]
fn an_invalid_pattern_selects_nothing_rather_than_panicking() {
    // `(` is outside the Claude literal charset, so it reaches the regex path.
    assert!(!matches_matcher(Some("("), "x", ClaudeCode));
    assert!(!matches_matcher(Some("["), "x", Codex));
}

/// TC-HOOK-MATCH-8: the diagnostic accepts every usable matcher.
#[test]
fn the_diagnostic_accepts_sentinels_literals_and_valid_regexes() {
    for (matcher, mode) in [
        (None, ClaudeCode),
        (Some(""), Codex),
        (Some("*"), Codex),
        (Some("Edit|Write"), ClaudeCode),
        (Some("^Bash$"), ClaudeCode),
        (Some("Edit|Write"), Codex),
    ] {
        assert_eq!(
            matcher_diagnostic(matcher, mode),
            None,
            "{matcher:?} under {mode:?} should be usable"
        );
    }
}

/// TC-HOOK-MATCH-9: an uncompilable pattern is refused with a message naming
/// the dialect and the pattern, so the configuration line can be found.
#[test]
fn the_diagnostic_names_the_dialect_and_the_pattern_it_refused() {
    assert_eq!(
        matcher_diagnostic(Some("("), ClaudeCode).as_deref(),
        Some(r#"invalid claude-code regex matcher "(""#)
    );
    assert_eq!(
        matcher_diagnostic(Some("["), Codex).as_deref(),
        Some(r#"invalid codex regex matcher "[""#)
    );
}

/// TC-HOOK-MATCH-10: a pattern JavaScript accepts and this engine does not is
/// refused at configuration time, not silently dead at match time.
///
/// This is the deliberate difference from upstream that the module note
/// describes: the `regex` crate has no lookaround, so `(?=x)` is valid under
/// `new RegExp` and invalid here. What matters is that the two answers agree
/// with each other — a pattern the matcher will never fire on is a pattern the
/// diagnostic refuses — because the failure a deployment cannot debug is a
/// hook that is configured, accepted, and never runs.
#[test]
fn a_pattern_this_engine_cannot_compile_is_refused_rather_than_silently_dead() {
    let lookahead = "foo(?=bar)";
    assert!(
        matcher_diagnostic(Some(lookahead), Codex).is_some(),
        "lookaround is not supported, so it must be refused"
    );
    assert!(
        !matches_matcher(Some(lookahead), "foobar", Codex),
        "and it must not match either"
    );
}

/// TC-HOOK-MATCH-11: the diagnostic and the matcher never disagree.
///
/// The property behind TC-HOOK-MATCH-10, over every pattern in the suite: if a
/// matcher is refused, it selects nothing at all; if it is accepted, it is
/// answerable without panicking. Stated as a case because the two functions
/// duplicate the sentinel and literal rules, and a change to one that is not
/// made to the other is exactly the drift a deployment would experience as a
/// hook that quietly stopped firing.
#[test]
fn a_refused_matcher_matches_nothing_and_an_accepted_one_answers() {
    let patterns = [
        None,
        Some(""),
        Some("*"),
        Some("Bash"),
        Some("Edit|Write"),
        Some("^Bash$"),
        Some(r".*\.ts$"),
        Some("("),
        Some("["),
        Some("foo(?=bar)"),
        Some(r"(\w+)\1"),
    ];
    let queries = ["Bash", "BashOutput", "Edit", "foo.ts", "foobar", ""];

    for mode in [ClaudeCode, Codex] {
        for pattern in patterns {
            let refused = matcher_diagnostic(pattern, mode).is_some();
            for query in queries {
                let selected = matches_matcher(pattern, query, mode);
                assert!(
                    !(refused && selected),
                    "{pattern:?} under {mode:?} was refused but still selected {query:?}"
                );
            }
        }
    }
}
