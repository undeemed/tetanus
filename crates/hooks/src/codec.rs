//! Reading what a hook process said.
//!
//! A hook answers in two channels at once: its exit status, and whatever it
//! printed. Decoding is deliberately *total* and lenient — every combination
//! of status and output produces a [`HookOutput`], and nothing here fails.
//! A hook is someone else's program, and a turn must not die because one of
//! them printed something unexpected.
//!
//! The exit status decides the frame:
//!
//! | exit | meaning |
//! | --- | --- |
//! | `0` | success; stdout *may* carry a structured answer |
//! | `2` | a block, with stderr as the reason |
//! | other | an error that does not block; stderr is kept as a diagnostic |
//! | none | could not be run at all |
//!
//! Exit 2 is authoritative: structured stdout is not even read, because a hook
//! that both blocked and printed an approval has contradicted itself and the
//! blocking channel is the one that fails closed.
//!
//! Parity: upstream `packages/hooks/hook-protocol/src/codec.ts`, pinned by its
//! `codec.spec.ts`.

use serde_json::{Map, Value};

use crate::types::{HookDecision, HookOutput};

/// The exit status both dialects read as "block, and here is why on stderr".
const BLOCKING_EXIT_CODE: i32 = 2;

/// Decode one hook process's result.
///
/// `expected_event` is the event that actually fired. When it is given, a
/// `hookSpecificOutput` block that names a different event — or names none —
/// has its event-scoped fields discarded, because a `PreToolUse` denial must
/// not deny a `Stop`. Passing `None` opts out of that guard and applies the
/// block as written.
pub fn parse_hook_output(
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    expected_event: Option<&str>,
) -> HookOutput {
    let stdout = stdout.trim();
    let stderr = stderr.trim();

    let mut output = HookOutput {
        exit_code,
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
        ..HookOutput::default()
    };

    if exit_code == Some(BLOCKING_EXIT_CODE) {
        output.decision = Some(HookDecision::Block);
        if !stderr.is_empty() {
            output.reason = Some(stderr.to_owned());
        }
        // Deliberately no structured parse: see the module note.
        return output;
    }

    // Structured output counts only on a clean exit, and only when it looks
    // like an object. Anything else is plain text the caller may still use,
    // not a malformed answer to complain about.
    if exit_code == Some(0) && stdout.starts_with('{') {
        if let Some(parsed) = serde_json::from_str::<Value>(stdout)
            .ok()
            .and_then(as_object)
        {
            apply_structured(&mut output, &parsed, expected_event);
        }
    }

    output
}

/// Fold a parsed structured answer into the outcome.
fn apply_structured(
    output: &mut HookOutput,
    parsed: &Map<String, Value>,
    expected_event: Option<&str>,
) {
    // Event-agnostic fields. These apply whatever event fired.
    if let Some(proceed) = boolean(parsed, "continue") {
        output.proceed = Some(proceed);
    }
    if let Some(reason) = string(parsed, "stopReason") {
        output.stop_reason = Some(reason.to_owned());
    }
    if let Some(message) = string(parsed, "systemMessage") {
        output.system_message = Some(message.to_owned());
    }
    if let Some(decision) = top_level_decision(string(parsed, "decision")) {
        output.decision = Some(decision);
    }
    if let Some(reason) = string(parsed, "reason") {
        output.reason = Some(reason.to_owned());
    }

    let Some(specific) = parsed
        .get("hookSpecificOutput")
        .and_then(|v| as_object(v.clone()))
    else {
        return;
    };

    // Recorded before the guard, so a discarded block can still be described.
    let claimed = string(&specific, "hookEventName").map(str::to_owned);
    output.hook_event_name.clone_from(&claimed);

    // A block that names the wrong event, or names none, cannot speak for the
    // event that fired. Under a keyed schema a missing discriminator is as
    // malformed as a wrong one.
    if let Some(expected) = expected_event {
        if claimed.as_deref() != Some(expected) {
            return;
        }
    }

    if let Some(decision) = permission_decision(string(&specific, "permissionDecision")) {
        output.decision = Some(decision);
    }
    if let Some(reason) = string(&specific, "permissionDecisionReason") {
        output.reason = Some(reason.to_owned());
    }
    if let Some(context) = string(&specific, "additionalContext") {
        output.additional_context = Some(context.to_owned());
    }
    if let Some(updated) = specific
        .get("updatedInput")
        .and_then(|v| as_object(v.clone()))
    {
        output.updated_input = Some(updated);
    }
}

/// The legacy top-level `decision`, which is `approve` or `block` and nothing
/// else.
///
/// `allow`, `deny` and `ask` are reserved for `permissionDecision` in both
/// reference schemas, so an out-of-band `{"decision":"deny"}` is malformed.
/// Ignoring it is the safe reading: honouring it would let a hook deny through
/// a channel the schema says cannot deny, bypassing the event guard below.
fn top_level_decision(value: Option<&str>) -> Option<HookDecision> {
    match value {
        Some("approve") => Some(HookDecision::Approve),
        Some("block") => Some(HookDecision::Block),
        _ => None,
    }
}

/// A `hookSpecificOutput.permissionDecision`, which is `allow`, `deny` or `ask`.
fn permission_decision(value: Option<&str>) -> Option<HookDecision> {
    match value {
        Some("allow") => Some(HookDecision::Allow),
        Some("deny") => Some(HookDecision::Deny),
        Some("ask") => Some(HookDecision::Ask),
        _ => None,
    }
}

/// A field that is present and a string. A wrong type reads as absent, because
/// a hook that sent a number where a reason belongs has not given a reason.
fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

/// A field that is present and a boolean.
fn boolean(object: &Map<String, Value>, key: &str) -> Option<bool> {
    object.get(key).and_then(Value::as_bool)
}

/// A plain JSON object. An array is not one, which is why this is not
/// `is_object`-and-index.
fn as_object(value: Value) -> Option<Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map),
        _ => None,
    }
}
