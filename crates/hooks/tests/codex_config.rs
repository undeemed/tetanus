//! Conformance: reading Codex's hook configuration.
//!
//! Feature under test: `tetanus_hooks::codex` — the other dialect's settings
//! format, and the four places it deliberately differs from Claude Code's.
//!
//! Ported from upstream `packages/hooks/hooks-codex/tests/config.spec.ts`.
//! Case ids TC-HOOK-CX-1..13. The last two are this port's own.

use serde_json::json;
use tetanus_hooks::claude_code::MatcherGroup;
use tetanus_hooks::codex::{parse_codex_config, ParsedCodexConfig, SkippedHook, CODEX_EVENTS};
use tetanus_hooks::runner::CommandHook;

fn parse(raw: serde_json::Value) -> ParsedCodexConfig {
    parse_codex_config(&raw).expect("a usable config")
}

fn cmd(command: &str) -> CommandHook {
    CommandHook {
        command: command.to_owned(),
        timeout_sec: None,
    }
}

fn skip(event: &str, reason: &str) -> SkippedHook {
    SkippedHook {
        event: event.to_owned(),
        reason: reason.to_owned(),
    }
}

/// TC-HOOK-CX-1: only the five served events survive.
///
/// `SubagentStop` is a real Codex event this adapter does not serve, and it is
/// dropped exactly like an event Codex never defined. Serving half of an event
/// would be worse than not serving it.
#[test]
fn only_the_five_served_events_survive() {
    let parsed = parse(json!({
        "PreToolUse": [{"hooks": [{"type": "command", "command": "a.sh"}]}],
        "SubagentStop": [{"hooks": [{"type": "command", "command": "b.sh"}]}],
        "Notification": [{"hooks": [{"type": "command", "command": "c.sh"}]}],
    }));

    let events: Vec<&str> = parsed
        .config
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(events, ["PreToolUse"]);
    assert!(CODEX_EVENTS.contains(&"PreToolUse"));
    assert!(!CODEX_EVENTS.contains(&"SubagentStop"));
}

/// TC-HOOK-CX-2: both timeout spellings are accepted, and nothing is
/// substituted — a `${VAR}` is the shell's business later.
#[test]
fn both_timeout_spellings_are_read_and_nothing_is_substituted() {
    let parsed = parse(json!({
        "Stop": [{"hooks": [
            {"type": "command", "command": "${NOT_SUBSTITUTED}/s.sh", "timeout": 10}
        ]}],
        "UserPromptSubmit": [{"hooks": [
            {"type": "command", "command": "u.sh", "timeoutSec": 20}
        ]}],
    }));

    assert_eq!(
        parsed.event("Stop"),
        Some(
            &[MatcherGroup {
                matcher: None,
                hooks: vec![CommandHook {
                    command: "${NOT_SUBSTITUTED}/s.sh".into(),
                    timeout_sec: Some(10),
                }],
            }][..]
        )
    );
    assert_eq!(
        parsed.event("UserPromptSubmit"),
        Some(
            &[MatcherGroup {
                matcher: None,
                hooks: vec![CommandHook {
                    command: "u.sh".into(),
                    timeout_sec: Some(20),
                }],
            }][..]
        )
    );
}

/// TC-HOOK-CX-3: an unsupported type and a background hook are both refused,
/// each with its own reason, and the synchronous one between them survives.
#[test]
fn unsupported_and_async_hooks_are_refused_with_their_reasons() {
    let parsed = parse(json!({
        "PreToolUse": [{"hooks": [
            {"type": "prompt"},
            {"type": "command", "command": "sync.sh"},
            {"type": "command", "command": "bg.sh", "async": true},
        ]}],
    }));

    assert_eq!(
        parsed.event("PreToolUse"),
        Some(
            &[MatcherGroup {
                matcher: None,
                hooks: vec![cmd("sync.sh")]
            }][..]
        )
    );
    assert_eq!(
        parsed.skipped,
        [
            skip("PreToolUse", "unsupported \"prompt\" hook"),
            skip("PreToolUse", "async hook"),
        ]
    );
}

/// TC-HOOK-CX-4: a wrapper and a bare map read the same.
#[test]
fn a_wrapper_and_a_bare_map_read_alike() {
    let groups = json!({"Stop": [{"hooks": [{"type": "command", "command": "s.sh"}]}]});
    assert_eq!(
        parse(groups.clone()).config,
        parse(json!({"hooks": groups})).config
    );
}

/// TC-HOOK-CX-5: malformed entries and a non-object document are dropped.
#[test]
fn malformed_entries_are_dropped_without_failing_the_parse() {
    assert!(parse(json!(null)).config.is_empty());
    assert!(parse(json!({"PreToolUse": "no"})).config.is_empty());
    assert!(parse(json!({
        "Stop": [7, {"hooks": "x"}, {"hooks": [{"type": "command", "command": 9}]}]
    }))
    .config
    .is_empty());
}

/// TC-HOOK-CX-6: junk inside a hooks array does not cost its valid sibling.
#[test]
fn junk_beside_a_valid_hook_does_not_cost_it() {
    let parsed = parse(json!({
        "Stop": [{"hooks": [null, 7, {"type": "command", "command": "s.sh"}]}]
    }));
    assert_eq!(
        parsed.event("Stop"),
        Some(
            &[MatcherGroup {
                matcher: None,
                hooks: vec![cmd("s.sh")]
            }][..]
        )
    );
}

/// TC-HOOK-CX-7: no type means command here too.
#[test]
fn a_hook_with_no_type_is_a_command() {
    let parsed = parse(json!({"Stop": [{"hooks": [{"command": "s.sh"}]}]}));
    assert_eq!(
        parsed.event("Stop"),
        Some(
            &[MatcherGroup {
                matcher: None,
                hooks: vec![cmd("s.sh")]
            }][..]
        )
    );
}

/// TC-HOOK-CX-8: a match-all group carries no matcher, and a matcher survives
/// when one is written.
#[test]
fn a_matcher_is_kept_when_present_and_absent_otherwise() {
    let all = parse(json!({"Stop": [{"hooks": [{"type": "command", "command": "s.sh"}]}]}));
    assert_eq!(
        all.event("Stop").and_then(|g| g[0].matcher.as_deref()),
        None
    );

    let some = parse(json!({
        "PreToolUse": [{"matcher": "^Bash$", "hooks": [{"type": "command", "command": "b.sh"}]}]
    }));
    assert_eq!(
        some.event("PreToolUse")
            .and_then(|g| g[0].matcher.as_deref()),
        Some("^Bash$")
    );
}

/// TC-HOOK-CX-9: an uncompilable matcher is fatal, and says which dialect
/// judged it — the message differs from Claude Code's on the same pattern.
#[test]
fn an_uncompilable_matcher_is_fatal_and_names_this_dialect() {
    let error = parse_codex_config(&json!({
        "PreToolUse": [{"matcher": "[", "hooks": [{"type": "command", "command": "s.sh"}]}]
    }))
    .expect_err("an invalid matcher must be refused");
    assert_eq!(
        error.to_string(),
        "invalid codex regex matcher \"[\" on event \"PreToolUse\""
    );
}

/// TC-HOOK-CX-10: a matcher on an event with nothing to match is discarded
/// before it is judged.
#[test]
fn a_matcher_on_an_event_without_a_subject_is_discarded_not_judged() {
    let parsed = parse(json!({
        "UserPromptSubmit": [{"matcher": "[", "hooks": [{"type": "command", "command": "prompt.sh"}]}],
        "Stop": [{"matcher": "(", "hooks": [{"type": "command", "command": "stop.sh"}]}],
    }));

    assert_eq!(
        parsed.event("UserPromptSubmit"),
        Some(
            &[MatcherGroup {
                matcher: None,
                hooks: vec![cmd("prompt.sh")]
            }][..]
        )
    );
    assert_eq!(
        parsed.event("Stop"),
        Some(
            &[MatcherGroup {
                matcher: None,
                hooks: vec![cmd("stop.sh")]
            }][..]
        )
    );
}

/// TC-HOOK-CX-11: a word pattern is a regex here, unlike the other dialect.
///
/// This port's own, and it is the dialect difference that actually changes
/// which hooks fire. The same configured `Bash` selects only `Bash` under
/// Claude Code and also `BashOutput` here; a parser that judged this
/// configuration with the wrong dialect would accept it and then match the
/// wrong tools at run time. Pinning it at the parse boundary is what stops the
/// two adapters sharing a mode by accident.
#[test]
fn this_dialect_judges_matchers_as_regexes() {
    // `(` is a valid Claude literal only if it were word-chars; it is not, so
    // both dialects reach the regex path and both refuse it. The difference
    // that matters is the dialect named in the message.
    let error = parse_codex_config(&json!({
        "PreToolUse": [{"matcher": "(", "hooks": [{"type": "command", "command": "s.sh"}]}]
    }))
    .expect_err("refused");
    assert!(
        error.to_string().starts_with("invalid codex regex matcher"),
        "got {error}"
    );

    // A bare word is a legal regex, so it is accepted here rather than being
    // treated as a literal alternation list.
    let parsed = parse(json!({
        "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "b.sh"}]}]
    }));
    assert_eq!(
        parsed
            .event("PreToolUse")
            .and_then(|g| g[0].matcher.as_deref()),
        Some("Bash")
    );
}

/// TC-HOOK-CX-12: a background hook is refused even when it is the only hook,
/// and the group goes with it.
///
/// This port's own. The interesting part is that refusing the hook empties the
/// group, and an empty group must not then be judged for its matcher — so a
/// background hook behind a bad matcher is two refusals that must not become
/// a fatal error.
#[test]
fn a_lone_async_hook_empties_its_group_without_failing_the_parse() {
    let parsed = parse_codex_config(&json!({
        "PreToolUse": [{"matcher": "(", "hooks": [
            {"type": "command", "command": "bg.sh", "async": true}
        ]}]
    }))
    .expect("an unrunnable group's matcher is never judged");

    assert!(parsed.config.is_empty());
    assert_eq!(parsed.skipped, [skip("PreToolUse", "async hook")]);
}

/// TC-HOOK-CX-13: `async: false` is not a refusal.
///
/// This port's own. Only the literal `true` means background; a hook that
/// wrote the field out explicitly to say "no" must run, and reading the key's
/// mere presence as a refusal would silently disable it.
#[test]
fn an_explicit_async_false_still_runs() {
    let parsed = parse(json!({
        "Stop": [{"hooks": [{"type": "command", "command": "s.sh", "async": false}]}]
    }));
    assert_eq!(
        parsed.event("Stop"),
        Some(
            &[MatcherGroup {
                matcher: None,
                hooks: vec![cmd("s.sh")]
            }][..]
        )
    );
    assert!(parsed.skipped.is_empty());
}
