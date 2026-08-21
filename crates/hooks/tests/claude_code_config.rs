//! Conformance: reading Claude Code's hook configuration.
//!
//! Feature under test: `tetanus_hooks::claude_code` — the parse from that
//! dialect's settings format into runnable matcher groups.
//!
//! Ported from upstream `packages/hooks/hooks-claude-code/tests/config.spec.ts`.
//! Case ids TC-HOOK-CC-1..13. The last two are this port's own.

use serde_json::json;
use tetanus_hooks::claude_code::{
    parse_claude_code_config, substitute_command, MatcherGroup, SkippedHook, SubstitutionVars,
};
use tetanus_hooks::runner::CommandHook;

/// Parse with no substitutions, expecting a usable config.
fn parse(raw: serde_json::Value) -> tetanus_hooks::claude_code::ParsedClaudeConfig {
    parse_claude_code_config(&raw, &SubstitutionVars::default()).expect("a usable config")
}

/// A command hook with no timeout.
fn cmd(command: &str) -> CommandHook {
    CommandHook {
        command: command.to_owned(),
        timeout_sec: None,
    }
}

/// TC-HOOK-CC-1: every occurrence of each set token is replaced.
#[test]
fn substitution_replaces_every_occurrence_of_each_token() {
    assert_eq!(
        substitute_command(
            "${CLAUDE_PLUGIN_ROOT}/x.sh",
            &SubstitutionVars {
                plugin_root: Some("/p".into()),
                project_dir: None,
            }
        ),
        "/p/x.sh"
    );
    assert_eq!(
        substitute_command(
            "${CLAUDE_PROJECT_DIR}/a ${CLAUDE_PROJECT_DIR}/b",
            &SubstitutionVars {
                plugin_root: None,
                project_dir: Some("/proj".into()),
            }
        ),
        "/proj/a /proj/b"
    );
    assert_eq!(
        substitute_command(
            "${CLAUDE_PLUGIN_ROOT}-${CLAUDE_PROJECT_DIR}",
            &SubstitutionVars {
                plugin_root: Some("/p".into()),
                project_dir: Some("/d".into()),
            }
        ),
        "/p-/d"
    );
}

/// TC-HOOK-CC-2: an unset token stays verbatim.
///
/// Replacing it with nothing would turn `${CLAUDE_PLUGIN_ROOT}/x` into `/x`,
/// which is a real path and quite a different one.
#[test]
fn an_unset_token_is_left_alone() {
    assert_eq!(
        substitute_command("${CLAUDE_PLUGIN_ROOT}/x", &SubstitutionVars::default()),
        "${CLAUDE_PLUGIN_ROOT}/x"
    );
}

/// TC-HOOK-CC-3: a settings file and a bare event map read the same.
#[test]
fn a_settings_wrapper_and_a_bare_event_map_read_alike() {
    let groups = json!({
        "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "x.sh"}]}]
    });
    let bare = parse(groups.clone());
    let wrapped = parse(json!({ "hooks": groups }));

    assert_eq!(bare.config, wrapped.config);
    assert_eq!(
        bare.event("PreToolUse"),
        Some(
            &[MatcherGroup {
                matcher: Some("Bash".into()),
                hooks: vec![cmd("x.sh")],
            }][..]
        )
    );
}

/// TC-HOOK-CC-4: a timeout carries across in seconds, and the command is
/// substituted as it is read.
#[test]
fn a_timeout_carries_across_and_the_command_is_substituted() {
    let parsed = parse_claude_code_config(
        &json!({
            "Stop": [{"hooks": [
                {"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/s.sh", "timeout": 30}
            ]}]
        }),
        &SubstitutionVars {
            plugin_root: Some("/p".into()),
            project_dir: None,
        },
    )
    .expect("a usable config");

    assert_eq!(
        parsed.event("Stop"),
        Some(
            &[MatcherGroup {
                matcher: None,
                hooks: vec![CommandHook {
                    command: "/p/s.sh".into(),
                    timeout_sec: Some(30),
                }],
            }][..]
        )
    );
}

/// TC-HOOK-CC-5: a hook type this harness cannot run is recorded, and the
/// runnable ones beside it survive.
#[test]
fn an_unrunnable_hook_type_is_recorded_and_its_neighbours_survive() {
    let parsed = parse(json!({
        "PreToolUse": [{"hooks": [
            {"type": "prompt", "prompt": "hi"},
            {"type": "command", "command": "ok.sh"},
            {"type": "http", "url": "http://x"},
        ]}]
    }));

    assert_eq!(
        parsed.event("PreToolUse"),
        Some(
            &[MatcherGroup {
                matcher: None,
                hooks: vec![cmd("ok.sh")],
            }][..]
        )
    );
    assert_eq!(
        parsed.skipped,
        [
            SkippedHook {
                event: "PreToolUse".into(),
                ty: "prompt".into()
            },
            SkippedHook {
                event: "PreToolUse".into(),
                ty: "http".into()
            },
        ]
    );
}

/// TC-HOOK-CC-6: no type means `command`, which is this dialect's default.
#[test]
fn a_hook_with_no_type_is_a_command() {
    let parsed = parse(json!({"Stop": [{"hooks": [{"command": "d.sh"}]}]}));
    assert_eq!(
        parsed.event("Stop"),
        Some(
            &[MatcherGroup {
                matcher: None,
                hooks: vec![cmd("d.sh")],
            }][..]
        )
    );
}

/// TC-HOOK-CC-7: malformed shapes are dropped, never fatal. A settings file is
/// hand-edited, and one bad stanza must not stop the harness booting.
#[test]
fn malformed_entries_are_dropped_without_failing_the_parse() {
    assert!(parse(json!({"PreToolUse": "nope"})).config.is_empty());
    assert!(parse(json!({
        "PreToolUse": [42, {"hooks": "no"}, {"hooks": [7, {"type": "command"}]}]
    }))
    .config
    .is_empty());
    // A group whose only hook has no command string has nothing runnable left.
    assert!(
        parse(json!({"Stop": [{"hooks": [{"type": "command", "command": 5}]}]}))
            .config
            .is_empty()
    );
}

/// TC-HOOK-CC-8: a document that is not an object holds no hooks.
#[test]
fn a_document_that_is_not_an_object_holds_nothing() {
    for raw in [json!(null), json!(42), json!([1, 2])] {
        assert!(parse(raw.clone()).config.is_empty(), "for {raw}");
    }
}

/// TC-HOOK-CC-9: a match-all group carries no matcher.
#[test]
fn a_match_all_group_has_no_matcher() {
    let parsed = parse(json!({"Stop": [{"hooks": [{"type": "command", "command": "s.sh"}]}]}));
    assert_eq!(
        parsed.event("Stop").and_then(|g| g[0].matcher.as_deref()),
        None
    );
}

/// TC-HOOK-CC-10: an uncompilable matcher on a runnable group is fatal, and
/// the message names the event.
///
/// This is the one strict rule in a lenient parser: that group's hooks would
/// never fire, and a hook a deployment believes is guarding something but
/// which cannot match is worse than no hook at all.
#[test]
fn an_uncompilable_matcher_on_a_runnable_group_is_fatal() {
    let error = parse_claude_code_config(
        &json!({
            "PreToolUse": [{"matcher": "(", "hooks": [{"type": "command", "command": "x.sh"}]}]
        }),
        &SubstitutionVars::default(),
    )
    .expect_err("an invalid matcher must be refused");
    assert_eq!(
        error.to_string(),
        "invalid claude-code regex matcher \"(\" on event \"PreToolUse\""
    );
}

/// TC-HOOK-CC-11: a matcher on an event with nothing to match is discarded
/// before it is judged.
///
/// It constrains nothing either way, so refusing it would fail a boot over a
/// field that could not have had an effect.
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

/// TC-HOOK-CC-12: an unsupported event is ignored whole, bad matcher and all,
/// without costing the supported hooks beside it.
#[test]
fn an_unsupported_event_is_ignored_without_harming_its_neighbours() {
    let parsed = parse(json!({
        "Setup": [{"matcher": "(", "hooks": [{"type": "command", "command": "ignored.sh"}]}],
        "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "kept.sh"}]}],
    }));

    assert_eq!(parsed.config.len(), 1, "only the supported event survives");
    assert_eq!(
        parsed.event("PreToolUse"),
        Some(
            &[MatcherGroup {
                matcher: Some("Bash".into()),
                hooks: vec![cmd("kept.sh")]
            }][..]
        )
    );
}

/// TC-HOOK-CC-13: a bad matcher on a group with nothing runnable is not fatal.
///
/// This port's own, and it pins the order of two rules that only interact
/// here: emptiness is checked before the matcher is judged. The group could
/// never have fired whatever its matcher said, so failing the boot over it
/// would refuse a configuration for a hook that does not exist.
#[test]
fn a_bad_matcher_on_an_empty_group_is_not_fatal() {
    let parsed = parse_claude_code_config(
        &json!({
            "PreToolUse": [
                {"matcher": "(", "hooks": [{"type": "prompt", "prompt": "not runnable"}]},
                {"matcher": "Bash", "hooks": [{"type": "command", "command": "kept.sh"}]},
            ]
        }),
        &SubstitutionVars::default(),
    )
    .expect("an unrunnable group's matcher is never judged");

    assert_eq!(
        parsed.event("PreToolUse"),
        Some(
            &[MatcherGroup {
                matcher: Some("Bash".into()),
                hooks: vec![cmd("kept.sh")]
            }][..]
        )
    );
    assert_eq!(parsed.skipped.len(), 1);
}

/// TC-HOOK-CC-14: substitution happens before the command is stored, so a
/// token can expand into a path containing spaces or another token's text
/// without being re-scanned.
///
/// This port's own. Substituting repeatedly, or in the other order, would let
/// a project directory whose name contains `${CLAUDE_PLUGIN_ROOT}` rewrite
/// itself — contrived as a path, but it is exactly the class of bug that makes
/// a config expand differently depending on which variable was set.
#[test]
fn substitution_does_not_rescan_what_it_produced() {
    let out = substitute_command(
        "${CLAUDE_PROJECT_DIR}/run.sh",
        &SubstitutionVars {
            plugin_root: Some("/should-not-appear".into()),
            project_dir: Some("/a ${CLAUDE_PLUGIN_ROOT} b".into()),
        },
    );
    assert_eq!(
        out, "/a ${CLAUDE_PLUGIN_ROOT} b/run.sh",
        "a token produced by a substitution is not itself substituted"
    );
}
