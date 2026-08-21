//! Conformance: the durable record that a hook ran, and what it decided.
//!
//! Feature under test: `tetanus_hooks::events` — the `hook/invoked` and
//! `hook/result` pair, and the stderr summary the result carries.
//!
//! Ported from upstream `packages/hooks/hook-protocol/tests/events.spec.ts`.
//! Case ids TC-HOOK-EVENT-1..12. The last two are this port's own.

use serde_json::json;
use tetanus_core::EventBus;
use tetanus_hooks::events::{
    append_hook_invoked, append_hook_result, summarize_stderr, HookDialect, HookInvocation,
    HookResultRecord, DEFAULT_STDERR_SUMMARY_MAX_CHARS,
};
use tetanus_hooks::types::{HookDecision, HookOutput};
use tetanus_session::{JsonlSessionLog, SessionEvent, SessionLog};

/// A journal in a directory that goes away with the case.
fn journal(name: &str) -> (tempfile::TempDir, std::sync::Arc<JsonlSessionLog>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(format!("{name}.jsonl"));
    let log = JsonlSessionLog::create(name, &path, EventBus::new()).expect("journal");
    (dir, log)
}

/// A hook that exited cleanly and said nothing.
fn quiet() -> HookOutput {
    HookOutput {
        exit_code: Some(0),
        ..HookOutput::default()
    }
}

/// The first event of a type on the journal.
fn first(log: &dyn SessionLog, ty: &str) -> SessionEvent {
    log.events()
        .into_iter()
        .find(|event| event.ty == ty)
        .unwrap_or_else(|| panic!("no {ty} event on the journal"))
}

/// A result record with the reference cap and a nominal duration.
fn result(handler: &str, output: HookOutput) -> HookResultRecord {
    HookResultRecord {
        turn: 1,
        point: "Stop".into(),
        handler_id: handler.into(),
        output,
        stderr_summary_max_chars: DEFAULT_STDERR_SUMMARY_MAX_CHARS,
        duration_ms: 5,
    }
}

/// TC-HOOK-EVENT-1: an invocation is recorded with the pattern that selected it.
#[test]
fn an_invocation_records_its_point_dialect_and_matcher() {
    let (_dir, log) = journal("invoked");
    append_hook_invoked(
        log.as_ref(),
        &HookInvocation {
            turn: 1,
            point: "PreToolUse".into(),
            dialect: HookDialect::ClaudeCode,
            handler_id: "h1".into(),
            matcher: Some("Bash".into()),
        },
    )
    .expect("append");

    let event = first(log.as_ref(), "hook/invoked");
    assert_eq!(
        event.data,
        json!({
            "turn": 1,
            "point": "PreToolUse",
            "dialect": "claude-code",
            "handlerId": "h1",
            "matcher": "Bash",
        })
    );
    assert_eq!(
        event.source_event_seqs, None,
        "a hook event is log-only, so it cites no sources"
    );
}

/// TC-HOOK-EVENT-2: a match-all hook omits the key rather than writing null.
/// "Matched everything" and "matched this pattern" are different facts.
#[test]
fn a_match_all_hook_omits_the_matcher_key() {
    let (_dir, log) = journal("matchall");
    append_hook_invoked(
        log.as_ref(),
        &HookInvocation {
            turn: 2,
            point: "Stop".into(),
            dialect: HookDialect::Codex,
            handler_id: "h2".into(),
            matcher: None,
        },
    )
    .expect("append");

    let event = first(log.as_ref(), "hook/invoked");
    assert_eq!(event.data.get("matcher"), None);
    assert_eq!(event.data["dialect"], json!("codex"));
}

/// TC-HOOK-EVENT-3: the result derives its durable fields from the output.
#[test]
fn a_result_derives_decision_exit_code_and_stderr_summary() {
    let (_dir, log) = journal("derived");
    append_hook_result(
        log.as_ref(),
        &HookResultRecord {
            point: "PreToolUse".into(),
            handler_id: "h1".into(),
            output: HookOutput {
                exit_code: Some(2),
                stderr: "blocked".into(),
                decision: Some(HookDecision::Deny),
                ..HookOutput::default()
            },
            ..result("h1", quiet())
        },
    )
    .expect("append");

    assert_eq!(
        first(log.as_ref(), "hook/result").data,
        json!({
            "turn": 1,
            "point": "PreToolUse",
            "handlerId": "h1",
            "decision": "deny",
            "exitCode": 2,
            "stderrSummary": "blocked",
            "durationMs": 5,
        })
    );
}

/// TC-HOOK-EVENT-4: a hook that could not run omits both derived keys, and
/// still records what it decided.
#[test]
fn a_result_with_no_exit_code_and_no_stderr_omits_both_keys() {
    let (_dir, log) = journal("sparse");
    append_hook_result(
        log.as_ref(),
        &result(
            "h3",
            HookOutput {
                exit_code: None,
                decision: Some(HookDecision::Allow),
                ..HookOutput::default()
            },
        ),
    )
    .expect("append");

    let data = first(log.as_ref(), "hook/result").data;
    assert_eq!(data.get("exitCode"), None);
    assert_eq!(data.get("stderrSummary"), None);
    assert_eq!(data["decision"], json!("allow"));
}

/// TC-HOOK-EVENT-5: every result carries a decision. A halt is `stop`, silence
/// is `pass`, and an explicit answer wins over the halt fallback.
#[test]
fn the_recorded_decision_falls_back_to_stop_then_pass() {
    let (_dir, log) = journal("fallback");
    let halted = HookOutput {
        proceed: Some(false),
        ..quiet()
    };
    let both = HookOutput {
        proceed: Some(false),
        decision: Some(HookDecision::Block),
        ..quiet()
    };
    for (handler, output) in [("halt", halted), ("noop", quiet()), ("both", both)] {
        append_hook_result(log.as_ref(), &result(handler, output)).expect("append");
    }

    let recorded: Vec<(String, String)> = log
        .events()
        .into_iter()
        .filter(|event| event.ty == "hook/result")
        .map(|event| {
            (
                event.data["handlerId"].as_str().unwrap_or_default().into(),
                event.data["decision"].as_str().unwrap_or_default().into(),
            )
        })
        .collect();
    assert_eq!(
        recorded,
        [
            ("halt".to_owned(), "stop".to_owned()),
            ("noop".to_owned(), "pass".to_owned()),
            ("both".to_owned(), "block".to_owned()),
        ]
    );
}

/// TC-HOOK-EVENT-6: the pair correlates by handler id.
#[test]
fn an_invocation_and_its_result_correlate_by_handler_id() {
    let (_dir, log) = journal("pair");
    append_hook_invoked(
        log.as_ref(),
        &HookInvocation {
            turn: 1,
            point: "PreToolUse".into(),
            dialect: HookDialect::ClaudeCode,
            handler_id: "pair-1".into(),
            matcher: None,
        },
    )
    .expect("append");
    append_hook_result(
        log.as_ref(),
        &result(
            "pair-1",
            HookOutput {
                decision: Some(HookDecision::Allow),
                ..quiet()
            },
        ),
    )
    .expect("append");

    assert_eq!(
        first(log.as_ref(), "hook/invoked").data["handlerId"],
        json!("pair-1")
    );
    assert_eq!(
        first(log.as_ref(), "hook/result").data["handlerId"],
        json!("pair-1")
    );
}

// ------------------------------------------------------------ stderr summary

/// TC-HOOK-EVENT-7: nothing printed is no summary, not an empty one.
#[test]
fn blank_stderr_has_no_summary() {
    assert_eq!(summarize_stderr("", 500), None);
    assert_eq!(summarize_stderr("  \n\t ", 500), None);
}

/// TC-HOOK-EVENT-8: a summary within the cap is kept, trimmed.
#[test]
fn a_summary_within_the_cap_is_kept_trimmed() {
    assert_eq!(
        summarize_stderr("  blocked: bad tool  ", 500).as_deref(),
        Some("blocked: bad tool")
    );
    assert_eq!(summarize_stderr("abc", 3).as_deref(), Some("abc"));
}

/// TC-HOOK-EVENT-9: past the cap it is cut and marked.
#[test]
fn a_summary_past_the_cap_is_cut_and_marked() {
    assert_eq!(summarize_stderr("abcdef", 4).as_deref(), Some("abcd…"));
    let long = "x".repeat(600);
    let expected = format!("{}…", "x".repeat(500));
    assert_eq!(summarize_stderr(&long, 500).as_deref(), Some(&*expected));
}

/// TC-HOOK-EVENT-10: the cap is exclusive — exactly the cap is kept verbatim.
#[test]
fn a_summary_exactly_at_the_cap_is_kept_verbatim() {
    let exact = "y".repeat(500);
    assert_eq!(summarize_stderr(&exact, 500).as_deref(), Some(&*exact));

    let (_dir, log) = journal("edge");
    append_hook_result(
        log.as_ref(),
        &result(
            "edge",
            HookOutput {
                exit_code: Some(2),
                stderr: format!("  {}  ", "x".repeat(600)),
                ..HookOutput::default()
            },
        ),
    )
    .expect("append");
    assert_eq!(
        first(log.as_ref(), "hook/result").data["stderrSummary"],
        json!(format!("{}…", "x".repeat(500)))
    );
}

/// TC-HOOK-EVENT-11: the cap counts characters, and a cut never lands inside
/// one.
///
/// This port's own. Slicing by bytes is the defect the tool-result pruner's
/// port had to fix, and stderr is the likeliest place for a hook to print
/// something outside ASCII. A cut mid-character would panic, and a cap counted
/// in bytes would cut a multi-byte summary short of what a reader expects.
#[test]
fn the_cap_counts_characters_and_never_cuts_one_in_half() {
    // Each of these is more than one byte, and the last is more than one
    // UTF-16 unit as well.
    for text in ["é", "字", "🙂"] {
        let long = text.repeat(600);
        let summary = summarize_stderr(&long, 500).expect("a summary");
        let body = summary.strip_suffix('…').expect("it was cut");
        assert_eq!(
            body.chars().count(),
            500,
            "{text:?} should be cut at 500 characters"
        );
        assert_eq!(
            body,
            text.repeat(500),
            "{text:?} should be cut on a character boundary"
        );
    }
}

/// TC-HOOK-EVENT-12: a zero cap keeps nothing but still reports there was
/// something.
///
/// This port's own. The cap arrives from configuration, so zero is reachable,
/// and `chars().take(0)` plus an ellipsis is the honest answer: the summary is
/// empty, and the mark says it was cut. What must not happen is silently
/// reporting no stderr at all, which would read as a hook that printed nothing.
#[test]
fn a_zero_cap_still_reports_that_something_was_printed() {
    assert_eq!(summarize_stderr("something", 0).as_deref(), Some("…"));
    assert_eq!(
        summarize_stderr("   ", 0),
        None,
        "but blank stderr is still no summary at all"
    );
}
