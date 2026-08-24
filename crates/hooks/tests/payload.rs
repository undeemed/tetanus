//! Conformance: what each dialect writes to a hook's stdin.
//!
//! Feature under test: `tetanus_hooks::payload` — the two dialects' payload
//! shapes, and the four places they describe the same event differently.
//!
//! Ported from the payload builders of upstream
//! `packages/hooks/hooks-claude-code/src/index.ts` and
//! `packages/hooks/hooks-codex/src/index.ts`, which upstream exercises through
//! its `bridge.spec.ts` suites.
//!
//! Case ids TC-HOOK-PAY-1..12. The last four are this port's own: upstream
//! asserts these shapes only incidentally, by driving a whole agent, so the
//! differences between the dialects are never stated in one place.

use serde_json::json;
use tetanus_hooks::payload::{
    claude_post_tool, claude_pre_tool, claude_prompt, claude_session_start, claude_stop,
    claude_subagent, codex_post_tool, codex_pre_tool, codex_prompt, codex_session_start,
    codex_stop, PayloadContext, ToolCallFacts, SUBAGENT_TYPE,
};

fn context() -> PayloadContext {
    PayloadContext {
        session_id: "s-1".into(),
        transcript_path: Some("/j/s-1.jsonl".into()),
        cwd: "/work".into(),
        turn: 3,
    }
}

fn call() -> ToolCallFacts {
    ToolCallFacts {
        tool_name: "Bash".into(),
        arguments: json!({"command": "ls -l", "timeout": 5}),
        tool_use_id: "call-1".into(),
    }
}

/// TC-HOOK-PAY-1: Claude Code's `SessionStart`.
#[test]
fn claude_session_start_names_its_source() {
    assert_eq!(
        claude_session_start(&context(), "startup"),
        json!({
            "session_id": "s-1",
            "transcript_path": "/j/s-1.jsonl",
            "cwd": "/work",
            "hook_event_name": "SessionStart",
            "source": "startup",
        })
    );
}

/// TC-HOOK-PAY-2: Claude Code's `UserPromptSubmit`.
#[test]
fn claude_prompt_carries_the_prompt_text() {
    assert_eq!(
        claude_prompt(&context(), "do the thing"),
        json!({
            "session_id": "s-1",
            "transcript_path": "/j/s-1.jsonl",
            "cwd": "/work",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "do the thing",
        })
    );
}

/// TC-HOOK-PAY-3: Claude Code passes the call's arguments through verbatim,
/// which is what lets a hook inspect any tool's input and not only a shell's.
#[test]
fn claude_pre_tool_passes_the_arguments_verbatim() {
    assert_eq!(
        claude_pre_tool(&context(), &call()),
        json!({
            "session_id": "s-1",
            "transcript_path": "/j/s-1.jsonl",
            "cwd": "/work",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "ls -l", "timeout": 5},
            "tool_use_id": "call-1",
        })
    );
}

/// TC-HOOK-PAY-4: `PostToolUse` adds what the tool produced.
#[test]
fn claude_post_tool_adds_the_response() {
    let payload = claude_post_tool(&context(), &call(), "total 0");
    assert_eq!(payload["tool_response"], json!("total 0"));
    assert_eq!(payload["hook_event_name"], json!("PostToolUse"));
    assert_eq!(
        payload["tool_input"],
        json!({"command": "ls -l", "timeout": 5})
    );
}

/// TC-HOOK-PAY-5: `Stop` carries the loop-guard flag, always false.
#[test]
fn claude_stop_says_it_is_not_a_reentry() {
    assert_eq!(
        claude_stop(&context()),
        json!({
            "session_id": "s-1",
            "transcript_path": "/j/s-1.jsonl",
            "cwd": "/work",
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
    );
}

/// TC-HOOK-PAY-6: the subagent pair is described by the child's context, and
/// only the stop half carries the loop-guard flag.
#[test]
fn the_subagent_pair_differs_only_by_the_loop_guard() {
    let child = PayloadContext {
        session_id: "child-1".into(),
        transcript_path: None,
        cwd: "/work/child".into(),
        turn: 0,
    };

    let started = claude_subagent(&child, "SubagentStart", "a-1");
    assert_eq!(
        started,
        json!({
            "session_id": "child-1",
            "transcript_path": "",
            "cwd": "/work/child",
            "hook_event_name": "SubagentStart",
            "agent_id": "a-1",
            "agent_type": SUBAGENT_TYPE,
        })
    );

    let stopped = claude_subagent(&child, "SubagentStop", "a-1");
    assert_eq!(stopped["stop_hook_active"], json!(false));
    assert_eq!(stopped["agent_type"], json!("general-purpose"));
}

/// TC-HOOK-PAY-7: every Codex payload carries the model and the permission
/// mode, which Claude Code's never do.
#[test]
fn every_codex_payload_carries_the_model_and_permission_mode() {
    let payload = codex_session_start(&context(), "deepseek-chat");
    assert_eq!(payload["model"], json!("deepseek-chat"));
    assert_eq!(payload["permission_mode"], json!("default"));
    assert_eq!(payload["hook_event_name"], json!("SessionStart"));
    assert_eq!(
        payload.get("turn_id"),
        None,
        "SessionStart is not turn-scoped"
    );
}

/// TC-HOOK-PAY-8: the turn-scoped Codex events carry `turn_id`, as a string.
#[test]
fn the_turn_scoped_codex_events_carry_a_string_turn_id() {
    for payload in [
        codex_prompt(&context(), "m", "go"),
        codex_pre_tool(&context(), "m", &call()),
        codex_post_tool(&context(), "m", &call(), "out"),
        codex_stop(&context(), "m"),
    ] {
        assert_eq!(
            payload["turn_id"],
            json!("3"),
            "turn_id is a string on the wire, in {}",
            payload["hook_event_name"]
        );
    }
}

/// TC-HOOK-PAY-9: Codex narrows `tool_input` to the command.
#[test]
fn codex_narrows_tool_input_to_the_command() {
    let payload = codex_pre_tool(&context(), "m", &call());
    assert_eq!(payload["tool_input"], json!({"command": "ls -l"}));
    assert_eq!(
        payload["tool_name"],
        json!("Bash"),
        "the tool name stays real, because it is what the matcher tests"
    );
}

/// TC-HOOK-PAY-10: an absent transcript is `""` in one dialect and `null` in
/// the other.
///
/// This port's own. It is one line in each builder and the kind of difference
/// that is silently lost in a rewrite; a hook that checks `transcript_path`
/// for truthiness behaves differently on each, and both behaviours are what
/// the respective ecosystem expects.
#[test]
fn an_absent_transcript_is_empty_in_one_dialect_and_null_in_the_other() {
    let fresh = PayloadContext {
        transcript_path: None,
        ..context()
    };
    assert_eq!(claude_stop(&fresh)["transcript_path"], json!(""));
    assert_eq!(codex_stop(&fresh, "m")["transcript_path"], json!(null));
}

/// TC-HOOK-PAY-11: a tool call with no command still produces the key.
///
/// This port's own. Codex hooks index into `tool_input.command`; an absent key
/// is a different failure from an empty command, and only one of them is what
/// a non-shell tool actually means.
#[test]
fn a_call_with_no_command_still_has_the_key_in_codex() {
    let no_command = ToolCallFacts {
        tool_name: "Read".into(),
        arguments: json!({"path": "/etc/hosts"}),
        tool_use_id: "call-2".into(),
    };
    assert_eq!(
        codex_pre_tool(&context(), "m", &no_command)["tool_input"],
        json!({"command": ""})
    );
    // The same call keeps everything under Claude Code, which is the point of
    // passing arguments through.
    assert_eq!(
        claude_pre_tool(&context(), &no_command)["tool_input"],
        json!({"path": "/etc/hosts"})
    );
}

/// TC-HOOK-PAY-12: arguments that are not an object do not break either
/// dialect.
///
/// This port's own. Arguments come from the model, and the codec's own suite
/// already establishes that a model writes malformed things; a payload builder
/// that panicked on one would fail the turn from a hook that was only watching.
#[test]
fn arguments_that_are_not_an_object_are_survivable() {
    for arguments in [json!(null), json!("just a string"), json!([1, 2]), json!(7)] {
        let odd = ToolCallFacts {
            tool_name: "Odd".into(),
            arguments: arguments.clone(),
            tool_use_id: "call-3".into(),
        };
        assert_eq!(
            codex_pre_tool(&context(), "m", &odd)["tool_input"],
            json!({"command": ""}),
            "codex, for {arguments}"
        );
        assert_eq!(
            claude_pre_tool(&context(), &odd)["tool_input"],
            arguments,
            "claude passes it through unchanged, for {arguments}"
        );
    }
}
