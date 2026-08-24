//! Conformance: reading what a hook process said.
//!
//! Feature under test: `tetanus_hooks::codec::parse_hook_output` — the total,
//! lenient decode of one hook's exit status and output.
//!
//! Ported from upstream `packages/hooks/hook-protocol/tests/codec.spec.ts`.
//! Case ids TC-HOOK-CODEC-1..22. The last two are this port's own.

use serde_json::json;
use tetanus_hooks::parse_hook_output;
use tetanus_hooks::types::HookDecision;

/// The common shape: a clean exit carrying a structured answer.
fn structured(value: serde_json::Value) -> String {
    value.to_string()
}

// ---------------------------------------------------------------- exit status

/// TC-HOOK-CODEC-1: a clean exit that printed nothing said nothing.
#[test]
fn a_clean_exit_with_no_output_is_neutral() {
    let out = parse_hook_output(Some(0), "", "", None);
    assert_eq!(out.exit_code, Some(0));
    assert_eq!(out.decision, None);
    assert_eq!(out.proceed, None);
}

/// TC-HOOK-CODEC-2: exit 2 blocks, and stderr is why.
#[test]
fn exit_two_blocks_with_stderr_as_the_reason() {
    let out = parse_hook_output(Some(2), "", "this command is not allowed", None);
    assert_eq!(out.decision, Some(HookDecision::Block));
    assert_eq!(out.reason.as_deref(), Some("this command is not allowed"));
    assert_eq!(out.stderr, "this command is not allowed");
}

/// TC-HOOK-CODEC-3: exit 2 still blocks when it explained nothing. The block
/// and its explanation are separate facts.
#[test]
fn exit_two_with_blank_stderr_still_blocks_with_no_reason() {
    let out = parse_hook_output(Some(2), "", "   ", None);
    assert_eq!(out.decision, Some(HookDecision::Block));
    assert_eq!(out.reason, None);
}

/// TC-HOOK-CODEC-4: any other failure is an error that does not block.
#[test]
fn another_non_zero_exit_is_an_error_that_does_not_block() {
    let out = parse_hook_output(Some(1), "", "some warning", None);
    assert_eq!(out.decision, None);
    assert_eq!(out.exit_code, Some(1));
    assert_eq!(out.stderr, "some warning");
}

/// TC-HOOK-CODEC-5: a hook that could not be run has not approved anything.
#[test]
fn a_hook_that_could_not_run_carries_no_decision() {
    let out = parse_hook_output(None, "", "spawn failed: ENOENT", None);
    assert_eq!(out.exit_code, None);
    assert_eq!(out.decision, None);
    assert_eq!(out.stderr, "spawn failed: ENOENT");
}

// ----------------------------------------------------------- structured stdout

/// TC-HOOK-CODEC-6: the event-agnostic top-level fields.
#[test]
fn the_top_level_control_fields_are_read() {
    let out = parse_hook_output(
        Some(0),
        &structured(json!({
            "continue": false,
            "stopReason": "budget exceeded",
            "systemMessage": "heads up",
        })),
        "",
        None,
    );
    assert_eq!(out.proceed, Some(false));
    assert_eq!(out.stop_reason.as_deref(), Some("budget exceeded"));
    assert_eq!(out.system_message.as_deref(), Some("heads up"));
}

/// TC-HOOK-CODEC-7: the legacy top-level decision, which is two spellings.
#[test]
fn the_legacy_top_level_decision_is_approve_or_block() {
    let blocked = parse_hook_output(
        Some(0),
        &structured(json!({"decision": "block", "reason": "nope"})),
        "",
        None,
    );
    assert_eq!(blocked.decision, Some(HookDecision::Block));

    let approved = parse_hook_output(
        Some(0),
        &structured(json!({"decision": "approve"})),
        "",
        None,
    );
    assert_eq!(approved.decision, Some(HookDecision::Approve));
}

/// TC-HOOK-CODEC-8: `allow`/`deny`/`ask` at the top level are malformed.
///
/// They belong to `permissionDecision`, which is the channel the event guard
/// protects. Honouring them here would be a way around that guard.
#[test]
fn a_permission_word_at_the_top_level_is_ignored() {
    for word in ["deny", "allow", "ask"] {
        let out = parse_hook_output(Some(0), &structured(json!({ "decision": word })), "", None);
        assert_eq!(
            out.decision, None,
            "top-level {word:?} must not take effect"
        );
    }
}

/// TC-HOOK-CODEC-9: the discriminator is recorded, and the block applies.
#[test]
fn the_claimed_event_name_is_recorded() {
    let out = parse_hook_output(
        Some(0),
        &structured(json!({
            "hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny"}
        })),
        "",
        None,
    );
    assert_eq!(out.hook_event_name.as_deref(), Some("PreToolUse"));
    assert_eq!(out.decision, Some(HookDecision::Deny));
}

/// TC-HOOK-CODEC-10: the per-event channel outranks the legacy one.
#[test]
fn a_permission_decision_overrides_the_legacy_decision() {
    let out = parse_hook_output(
        Some(0),
        &structured(json!({
            "decision": "approve",
            "hookSpecificOutput": {
                "permissionDecision": "deny",
                "permissionDecisionReason": "denied by policy",
            },
        })),
        "",
        None,
    );
    assert_eq!(out.decision, Some(HookDecision::Deny));
    assert_eq!(out.reason.as_deref(), Some("denied by policy"));
}

/// TC-HOOK-CODEC-11: all three permission words are read.
#[test]
fn allow_and_ask_permission_decisions_are_read() {
    for (word, expected) in [
        ("allow", HookDecision::Allow),
        ("ask", HookDecision::Ask),
        ("deny", HookDecision::Deny),
    ] {
        let out = parse_hook_output(
            Some(0),
            &structured(json!({"hookSpecificOutput": {"permissionDecision": word}})),
            "",
            None,
        );
        assert_eq!(out.decision, Some(expected), "permissionDecision {word:?}");
    }
}

/// TC-HOOK-CODEC-12: the two payload fields of the per-event channel.
#[test]
fn additional_context_and_updated_input_are_read() {
    let out = parse_hook_output(
        Some(0),
        &structured(json!({
            "hookSpecificOutput": {
                "additionalContext": "remember X",
                "updatedInput": {"command": "safe"},
            },
        })),
        "",
        None,
    );
    assert_eq!(out.additional_context.as_deref(), Some("remember X"));
    assert_eq!(
        out.updated_input.map(serde_json::Value::Object),
        Some(json!({"command": "safe"}))
    );
}

/// TC-HOOK-CODEC-13: a word nobody defined is not coerced into one that exists.
#[test]
fn an_unknown_decision_word_is_ignored() {
    let out = parse_hook_output(Some(0), &structured(json!({"decision": "maybe"})), "", None);
    assert_eq!(out.decision, None);
}

// ------------------------------------------------------------- the event guard

/// TC-HOOK-CODEC-14: a block naming another event has its event-scoped fields
/// discarded. A stray `PreToolUse` denial must not deny a `Stop`.
#[test]
fn a_block_naming_another_event_is_discarded_but_still_recorded() {
    let out = parse_hook_output(
        Some(0),
        &structured(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": "no",
                "additionalContext": "x",
                "updatedInput": {"command": "y"},
            },
        })),
        "",
        Some("Stop"),
    );
    assert_eq!(
        out.hook_event_name.as_deref(),
        Some("PreToolUse"),
        "what the block claimed is kept, so a diagnostic can say so"
    );
    assert_eq!(out.decision, None);
    assert_eq!(out.reason, None);
    assert_eq!(out.additional_context, None);
    assert_eq!(out.updated_input, None);
}

/// TC-HOOK-CODEC-15: a block naming the firing event applies.
#[test]
fn a_block_naming_the_firing_event_applies() {
    let out = parse_hook_output(
        Some(0),
        &structured(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "additionalContext": "x",
            },
        })),
        "",
        Some("PreToolUse"),
    );
    assert_eq!(out.decision, Some(HookDecision::Deny));
    assert_eq!(out.additional_context.as_deref(), Some("x"));
}

/// TC-HOOK-CODEC-16: with no firing event given, the guard is off.
#[test]
fn the_guard_is_opt_out_when_no_firing_event_is_given() {
    let out = parse_hook_output(
        Some(0),
        &structured(json!({
            "hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny"},
        })),
        "",
        None,
    );
    assert_eq!(out.decision, Some(HookDecision::Deny));
}

/// TC-HOOK-CODEC-17: a block with no discriminator is as malformed as a wrong
/// one, when an event is expected.
#[test]
fn a_block_with_no_event_name_is_discarded_when_an_event_is_expected() {
    let out = parse_hook_output(
        Some(0),
        &structured(json!({
            "hookSpecificOutput": {"permissionDecision": "deny", "additionalContext": "x"},
        })),
        "",
        Some("Stop"),
    );
    assert_eq!(out.hook_event_name, None);
    assert_eq!(out.decision, None);
    assert_eq!(out.additional_context, None);
}

/// TC-HOOK-CODEC-18: with the guard off, a discriminator-less block applies.
#[test]
fn a_block_with_no_event_name_applies_when_the_guard_is_off() {
    let out = parse_hook_output(
        Some(0),
        &structured(json!({"hookSpecificOutput": {"permissionDecision": "deny"}})),
        "",
        None,
    );
    assert_eq!(out.decision, Some(HookDecision::Deny));
}

/// TC-HOOK-CODEC-19: only the per-event block is scoped. Discarding it must
/// not discard the event-agnostic fields beside it.
#[test]
fn a_discarded_block_does_not_take_the_top_level_fields_with_it() {
    let out = parse_hook_output(
        Some(0),
        &structured(json!({
            "decision": "block",
            "reason": "top",
            "continue": false,
            "stopReason": "halt",
            "hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow"},
        })),
        "",
        Some("Stop"),
    );
    assert_eq!(out.decision, Some(HookDecision::Block));
    assert_eq!(out.reason.as_deref(), Some("top"));
    assert_eq!(out.proceed, Some(false));
    assert_eq!(out.stop_reason.as_deref(), Some("halt"));
}

// ------------------------------------------------------------------- leniency

/// TC-HOOK-CODEC-20: output that is not the JSON it looked like is not an error.
#[test]
fn malformed_json_on_a_clean_exit_is_not_an_error() {
    let out = parse_hook_output(Some(0), "{ not valid json", "", None);
    assert_eq!(out.decision, None);
    assert_eq!(out.proceed, None);
}

/// TC-HOOK-CODEC-21: plain text is plain text, kept verbatim after trimming,
/// and an array is not an object.
#[test]
fn plain_text_and_arrays_are_left_alone_and_stdout_is_preserved() {
    let text = parse_hook_output(Some(0), "just some text output", "", None);
    assert_eq!(text.decision, None);
    assert_eq!(text.stdout, "just some text output");

    let array = parse_hook_output(Some(0), "[1,2,3]", "", None);
    assert_eq!(array.decision, None);

    let empty = parse_hook_output(Some(0), "", "", None);
    assert_eq!(empty.stdout, "");

    let json = structured(json!({"decision": "block"}));
    let padded = parse_hook_output(Some(0), &format!("  {json}  \n"), "", None);
    assert_eq!(padded.stdout, json, "kept verbatim, trimmed");
    assert_eq!(padded.decision, Some(HookDecision::Block));
}

/// TC-HOOK-CODEC-22: exit 2 is authoritative — a hook that blocked and printed
/// an approval has contradicted itself, and the blocking channel wins.
#[test]
fn structured_output_is_ignored_on_a_blocking_exit() {
    let out = parse_hook_output(
        Some(2),
        &structured(json!({"decision": "approve"})),
        "blocked",
        None,
    );
    assert_eq!(out.decision, Some(HookDecision::Block));
    assert_eq!(out.reason.as_deref(), Some("blocked"));
}

/// TC-HOOK-CODEC-23: decoding is total. No input panics, whatever the hook
/// printed.
///
/// This port's own. A hook is someone else's program and its output is
/// untrusted input; the reason decoding is lenient rather than validating is
/// that a turn must survive it. Wrong types where strings and objects are
/// expected are the interesting half — upstream's `str`/`bool`/`obj` helpers
/// read those as absent, and nothing upstream asks whether that is so.
#[test]
fn decoding_is_total_over_hostile_output() {
    let payloads = [
        String::from("{"),
        String::from("{}"),
        String::from("[]"),
        String::from("null"),
        String::from("{\"continue\": \"not a bool\"}"),
        String::from("{\"reason\": 42}"),
        String::from("{\"decision\": null}"),
        structured(json!({"hookSpecificOutput": "not an object"})),
        structured(json!({"hookSpecificOutput": {"hookEventName": 7}})),
        structured(json!({"hookSpecificOutput": {"updatedInput": [1, 2]}})),
        structured(json!({"hookSpecificOutput": {"additionalContext": {"a": 1}}})),
        "\u{0}\u{1}invalid utf8-ish \u{feff}".to_owned(),
    ];
    for exit in [None, Some(0), Some(1), Some(2)] {
        for payload in &payloads {
            let out = parse_hook_output(exit, payload, "stderr text", Some("Stop"));
            // A wrong-typed field is absent, never coerced.
            assert!(
                out.reason.is_none() || exit == Some(2),
                "{payload:?} at {exit:?} invented a reason"
            );
            assert_eq!(out.exit_code, exit);
        }
    }
}

/// TC-HOOK-CODEC-24: the event guard cannot be bypassed through the legacy
/// channel.
///
/// This port's own, and the reason TC-HOOK-CODEC-8 matters. The guard protects
/// `permissionDecision`. If the top level also accepted `deny`, a hook could
/// deny an event it was not fired for by writing the word one line higher.
#[test]
fn the_event_guard_cannot_be_bypassed_by_writing_deny_at_the_top_level() {
    let out = parse_hook_output(
        Some(0),
        &structured(json!({
            "decision": "deny",
            "hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny"},
        })),
        "",
        Some("Stop"),
    );
    assert_eq!(
        out.decision, None,
        "neither channel may deny an event this block was not for"
    );
}
