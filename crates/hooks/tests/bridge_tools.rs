//! Conformance: the two tool hook points, registered against a running turn.
//!
//! Feature under test: `tetanus_hooks::bridge` — the part that binds this
//! crate's pure decisions to `crates/turn`'s extension points. `PreToolUse`
//! rewrites a call and may forbid it; `PostToolUse` puts text in front of the
//! model without touching the tool's answer.
//!
//! Ported from the registration half of upstream
//! `packages/hooks/hooks-claude-code/src/index.ts` and
//! `packages/hooks/hooks-codex/src/index.ts`, whose own `bridge.spec.ts`
//! suites drive a whole agent. Case ids TC-HOOK-BRIDGE-1..10.
//!
//! Approach: a recording executor rather than a real one, exactly as
//! `runner.rs` does and for the same reason - the real executor is
//! `crates/exec`'s and has its own suite. The bridge's own logic is asserted
//! directly against the seam's inputs and outputs, so a case says what a hook
//! answered and what the pipeline then saw.
//!
//! What is not restated, and why. Upstream's bridge suites register against a
//! live agent and assert the model's transcript; the equivalent end-to-end
//! assertion for the context half already exists at
//! `crates/turn/tests/upstream_post_tool_context.rs`, which pins that a
//! context reaches the model as its own message. Repeating it here would test
//! the turn engine twice and the bridge once.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tetanus_hooks::bridge::{
    apply_updated_input, contexts_from, permission_from, BridgeConfig, PendingDecisions, ToolHooks,
};
use tetanus_hooks::events::HookDialect;
use tetanus_hooks::payload::PayloadContext;
use tetanus_hooks::runner::{CommandHook, HookExecResult, HookExecSpec, HookExecutor};
use tetanus_hooks::types::HookOutput;
use tetanus_hooks::MatcherGroup;
use tetanus_session::{JsonlSessionLog, SessionLog};
use tetanus_turn::tools::{Permission, ToolCall};

/// An executor that answers each call from a queue and records what it was
/// asked, so a case can assert both the payload and the fold.
struct Scripted {
    specs: Mutex<Vec<HookExecSpec>>,
    answers: Mutex<Vec<HookExecResult>>,
}

impl Scripted {
    fn new(answers: Vec<HookExecResult>) -> Arc<Self> {
        Arc::new(Self {
            specs: Mutex::new(Vec::new()),
            answers: Mutex::new(answers),
        })
    }
    fn calls(&self) -> Vec<HookExecSpec> {
        self.specs.lock().expect("specs").clone()
    }
}

impl HookExecutor for Scripted {
    fn run<'a>(
        &'a self,
        spec: HookExecSpec,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<HookExecResult, String>> + Send + 'a>,
    > {
        self.specs.lock().expect("specs").push(spec);
        let mut queue = self.answers.lock().expect("answers");
        let answer = if queue.is_empty() {
            HookExecResult {
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            }
        } else {
            queue.remove(0)
        };
        Box::pin(async move { Ok(answer) })
    }
}

fn said(json_text: &str) -> HookExecResult {
    HookExecResult {
        exit_code: Some(0),
        stdout: json_text.to_owned(),
        stderr: String::new(),
    }
}

fn group(matcher: Option<&str>) -> MatcherGroup {
    MatcherGroup {
        matcher: matcher.map(str::to_owned),
        hooks: vec![CommandHook {
            command: "a-hook".into(),
            timeout_sec: None,
        }],
    }
}

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "call-1".into(),
        name: name.into(),
        arguments,
    }
}

struct Bridge {
    hooks: ToolHooks,
    executor: Arc<Scripted>,
    log: Arc<JsonlSessionLog>,
    _dir: tempfile::TempDir,
}

fn bridge(
    dialect: HookDialect,
    answers: Vec<HookExecResult>,
    config: impl FnOnce(&mut BridgeConfig),
) -> Bridge {
    let dir = tempfile::tempdir().expect("temp dir");
    let log = JsonlSessionLog::create(
        "bridge",
        dir.path().join("s.jsonl"),
        tetanus_core::EventBus::new(),
    )
    .expect("journal");
    let mut cfg = BridgeConfig::new(
        dialect,
        PayloadContext {
            session_id: "s1".into(),
            transcript_path: None,
            cwd: "/w".into(),
            turn: 1,
        },
    );
    config(&mut cfg);
    let executor = Scripted::new(answers);
    Bridge {
        hooks: ToolHooks {
            config: cfg,
            executor: Arc::clone(&executor) as Arc<dyn HookExecutor>,
            log: Arc::clone(&log) as Arc<dyn SessionLog>,
            pending: Arc::new(PendingDecisions::default()),
            clock: Arc::new(|| 0),
        },
        executor,
        log,
        _dir: dir,
    }
}

/// TC-HOOK-BRIDGE-1: a hook that says nothing changes nothing.
///
/// The common case by a wide margin: most hooks observe. A bridge that turned
/// silence into an answer would make every deployment's first hook a policy
/// change nobody asked for.
#[tokio::test]
async fn a_silent_hook_leaves_the_call_and_the_permission_alone() {
    let b = bridge(HookDialect::ClaudeCode, vec![said("")], |c| {
        c.pre_tool_use = vec![group(None)];
    });
    let mut c = call("write", json!({"path": "a.txt"}));

    b.hooks.pre_tool_use(1, &mut c).await;

    assert_eq!(c.arguments, json!({"path": "a.txt"}), "not rewritten");
    assert_eq!(
        b.hooks.gate("call-1", Permission::Allow),
        Permission::Allow,
        "and not gated"
    );
}

/// TC-HOOK-BRIDGE-2: a hook may rewrite the call that then runs.
///
/// The rewrite has to land at `tools/pre-execute`, whose answer *is* the call
/// the pipeline goes on to gate and dispatch. A rewrite applied later would be
/// judged against arguments that were never going to run.
#[tokio::test]
async fn a_hook_can_rewrite_the_arguments_before_the_call_runs() {
    let b = bridge(
        HookDialect::ClaudeCode,
        vec![said(
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","updatedInput":{"path":"safe.txt"}}}"#,
        )],
        |c| c.pre_tool_use = vec![group(None)],
    );
    let mut c = call("write", json!({"path": "danger.txt"}));

    b.hooks.pre_tool_use(1, &mut c).await;

    assert_eq!(c.arguments, json!({"path": "safe.txt"}));
}

/// TC-HOOK-BRIDGE-3: a hook that forbids a call denies it, in its own words.
///
/// The path that did not exist before the permission seam was routed. A hook
/// has no human behind it, so its refusal must not be staged as a question:
/// it is already the answer.
#[tokio::test]
async fn a_forbidding_hook_denies_the_call_with_its_reason() {
    let b = bridge(
        HookDialect::ClaudeCode,
        vec![said(
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"not on production"}}"#,
        )],
        |c| c.pre_tool_use = vec![group(None)],
    );
    let mut c = call("write", json!({}));

    b.hooks.pre_tool_use(1, &mut c).await;

    assert_eq!(
        b.hooks.gate("call-1", Permission::Allow),
        Permission::deny("not on production")
    );
}

/// TC-HOOK-BRIDGE-4: a permitting hook cannot un-gate what the tool gated.
///
/// A hook saying "I permit this" is not a hook saying "and nobody else may
/// object". Letting it lower the declared answer would let a deployment's
/// convenience hook quietly disarm the gate a tool author put on an
/// irreversible call - and the author is the one who knows it is irreversible.
#[tokio::test]
async fn a_permitting_hook_does_not_lower_the_declared_permission() {
    let b = bridge(
        HookDialect::ClaudeCode,
        vec![said(
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#,
        )],
        |c| c.pre_tool_use = vec![group(None)],
    );
    let mut c = call("rm", json!({}));

    b.hooks.pre_tool_use(1, &mut c).await;

    assert_eq!(
        b.hooks
            .gate("call-1", Permission::ask("this deletes things")),
        Permission::ask("this deletes things"),
        "the tool's own gate stands"
    );
}

/// TC-HOOK-BRIDGE-5: a bridge that did not run is not an approval.
///
/// Absence has to be inert. If a missing answer read as permission, then every
/// way a bridge can fail to run - a crash, a misconfiguration, a call that
/// never reached the gate - would silently open it.
#[tokio::test]
async fn a_missing_answer_leaves_the_declared_permission_untouched() {
    let b = bridge(HookDialect::ClaudeCode, vec![], |_| {});
    assert_eq!(
        b.hooks.gate("never-ran", Permission::ask("gated")),
        Permission::ask("gated")
    );
}

/// TC-HOOK-BRIDGE-6: one hook run decides one call.
///
/// The held answer is taken exactly once. If the gate could read it twice, a
/// second call reusing the id - or a retry - would be judged by a hook run
/// that was about a different call.
#[tokio::test]
async fn a_held_answer_is_taken_once() {
    let b = bridge(
        HookDialect::ClaudeCode,
        vec![said(
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"no"}}"#,
        )],
        |c| c.pre_tool_use = vec![group(None)],
    );
    let mut c = call("write", json!({}));
    b.hooks.pre_tool_use(1, &mut c).await;

    assert_eq!(
        b.hooks.gate("call-1", Permission::Allow),
        Permission::deny("no")
    );
    assert_eq!(
        b.hooks.gate("call-1", Permission::Allow),
        Permission::Allow,
        "the second read finds nothing, rather than deciding again"
    );
    assert!(b.hooks.pending.is_empty(), "and nothing is left held");
}

/// TC-HOOK-BRIDGE-7: only the hooks whose matcher selects the tool run.
///
/// A matcher that fired for everything would run a deployment's `Bash` hook on
/// every file read, which is both a performance problem and a correctness one:
/// the hook would be reading a payload it was never written for.
#[tokio::test]
async fn only_matching_hooks_run() {
    let b = bridge(HookDialect::ClaudeCode, vec![said(""), said("")], |c| {
        c.pre_tool_use = vec![group(Some("Bash")), group(Some("Write"))];
    });
    let mut c = call("Write", json!({}));

    b.hooks.pre_tool_use(1, &mut c).await;

    assert_eq!(b.executor.calls().len(), 1, "one of the two groups matched");
}

/// TC-HOOK-BRIDGE-8: every run leaves an audit pair on the journal.
///
/// `hook/invoked` is written *before* the run and `hook/result` after, so a
/// hook that hangs still leaves a record. Without the leading event a reader
/// cannot tell a hook that never returned from a hook that was never selected.
#[tokio::test]
async fn each_run_writes_its_audit_pair() {
    let b = bridge(HookDialect::ClaudeCode, vec![said("")], |c| {
        c.pre_tool_use = vec![group(None)];
    });
    let mut c = call("write", json!({}));

    b.hooks.pre_tool_use(1, &mut c).await;

    let types: Vec<String> = b.log.events().into_iter().map(|e| e.ty).collect();
    assert_eq!(types, ["hook/invoked", "hook/result"]);
}

/// TC-HOOK-BRIDGE-9: a `PostToolUse` hook's text becomes messages, one each.
///
/// Each hook is a different program with a different thing to say. Joining
/// their notes into one blob would invent a single voice for several unrelated
/// remarks, and a reader could not tell which hook said what.
#[tokio::test]
async fn post_tool_context_becomes_one_message_per_hook() {
    let b = bridge(
        HookDialect::ClaudeCode,
        vec![
            said(
                r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"first"}}"#,
            ),
            said(
                r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"second"}}"#,
            ),
        ],
        |c| {
            c.post_tool_use = vec![group(None), group(None)];
        },
    );
    let c = call("write", json!({}));

    let contexts = b.hooks.post_tool_use(1, &c, "the tool's answer").await;
    let texts: Vec<String> = contexts.into_iter().map(|m| m.content).collect();
    assert_eq!(texts, ["first", "second"]);
}

/// TC-HOOK-BRIDGE-10: the pure mappings, stated directly.
///
/// The three folds the listeners are built from, asserted without a turn so a
/// failure names the mapping rather than the wiring.
#[test]
fn the_mappings_between_a_hook_answer_and_the_pipeline() {
    use tetanus_hooks::types::{MergedDecision, MergedHookOutcome};

    // Nothing said, nothing changed - and an `Allow` is also nothing changed.
    for quiet in [MergedDecision::None, MergedDecision::Allow] {
        assert_eq!(
            permission_from(&MergedHookOutcome {
                decision: quiet,
                ..Default::default()
            }),
            None
        );
    }
    // A hook that forbids without saying why still refuses, in words a person
    // can read rather than in silence.
    let bare = permission_from(&MergedHookOutcome {
        decision: MergedDecision::Deny,
        ..Default::default()
    });
    assert!(matches!(bare, Some(Permission::Deny { .. })));

    // Empty context text contributes no message: a hook that wrote a blank
    // line should not put a blank turn in front of the model.
    assert!(contexts_from(&MergedHookOutcome {
        additional_context: vec!["  ".into(), String::new()],
        ..Default::default()
    })
    .is_empty());

    // The last rewrite wins, because hooks run in configuration order.
    let mut c = call("write", json!({"n": 0}));
    let rewrite = |n: i32| HookOutput {
        updated_input: Some(json!({ "n": n }).as_object().expect("an object").clone()),
        ..Default::default()
    };
    apply_updated_input(&mut c, &[rewrite(1), rewrite(2)]);
    assert_eq!(c.arguments, json!({"n": 2}));
}
