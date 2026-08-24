//! Conformance: the three observation hook points.
//!
//! Feature under test: `tetanus_hooks::bridge`'s `SessionStart`,
//! `UserPromptSubmit` and `Stop` - the points that carry no permission answer
//! and therefore cannot refuse a tool call. Two of them can still change what
//! happens, and which two is the whole of what these cases pin.
//!
//! Ported from the registration half of upstream
//! `packages/hooks/hooks-claude-code/src/index.ts` and
//! `packages/hooks/hooks-codex/src/index.ts`. Case ids
//! TC-HOOK-WATCH-1..10.
//!
//! Approach: a recording executor, as `runner.rs` and `bridge_tools.rs` use,
//! so a case says what a hook answered and what the pipeline then saw.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value.

use std::sync::{Arc, Mutex};

use tetanus_hooks::bridge::{prompt_refusal, stop_veto, BridgeConfig, WatchHooks};
use tetanus_hooks::events::HookDialect;
use tetanus_hooks::payload::PayloadContext;
use tetanus_hooks::runner::{CommandHook, HookExecResult, HookExecSpec, HookExecutor};
use tetanus_hooks::types::{MergedDecision, MergedHookOutcome};
use tetanus_hooks::MatcherGroup;
use tetanus_session::{JsonlSessionLog, SessionLog};

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

fn said(stdout: &str) -> HookExecResult {
    HookExecResult {
        exit_code: Some(0),
        stdout: stdout.to_owned(),
        stderr: String::new(),
    }
}

fn group() -> MatcherGroup {
    MatcherGroup {
        matcher: None,
        hooks: vec![CommandHook {
            command: "a-hook".into(),
            timeout_sec: None,
        }],
    }
}

struct Fixture {
    hooks: WatchHooks,
    executor: Arc<Scripted>,
    log: Arc<JsonlSessionLog>,
    _dir: tempfile::TempDir,
}

fn fixture(dialect: HookDialect, answers: Vec<HookExecResult>, points: usize) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let log = JsonlSessionLog::create(
        "watch",
        dir.path().join("s.jsonl"),
        tetanus_core::EventBus::new(),
    )
    .expect("journal");
    let config = BridgeConfig::new(
        dialect,
        PayloadContext {
            session_id: "s1".into(),
            transcript_path: None,
            cwd: "/w".into(),
            turn: 1,
        },
    );
    let executor = Scripted::new(answers);
    let groups = vec![group(); points];
    Fixture {
        hooks: WatchHooks {
            config,
            executor: Arc::clone(&executor) as Arc<dyn HookExecutor>,
            log: Arc::clone(&log) as Arc<dyn SessionLog>,
            clock: Arc::new(|| 0),
            session_start: groups.clone(),
            user_prompt_submit: groups.clone(),
            stop: groups,
        },
        executor,
        log,
        _dir: dir,
    }
}

/// TC-HOOK-WATCH-1: each point writes the payload its dialect defines.
///
/// The payload is the whole contract with a hook program, so the case that
/// matters most is that the right one reaches stdin at the right point. A
/// hook reading `prompt` at `Stop` would find nothing and conclude the user
/// said nothing.
#[tokio::test]
async fn each_point_writes_its_own_payload() {
    let f = fixture(HookDialect::ClaudeCode, vec![], 1);

    f.hooks.session_start("startup").await;
    f.hooks.user_prompt_submit(1, "do the thing").await;
    f.hooks.stop(1).await;

    let payloads: Vec<serde_json::Value> = f
        .executor
        .calls()
        .into_iter()
        .map(|spec| serde_json::from_str(&spec.stdin).expect("json on stdin"))
        .collect();
    assert_eq!(payloads[0]["hook_event_name"], "SessionStart");
    assert_eq!(payloads[0]["source"], "startup");
    assert_eq!(payloads[1]["hook_event_name"], "UserPromptSubmit");
    assert_eq!(payloads[1]["prompt"], "do the thing");
    assert_eq!(payloads[2]["hook_event_name"], "Stop");
    assert_eq!(payloads[2]["stop_hook_active"], false);
}

/// TC-HOOK-WATCH-2: Codex's payloads differ, and the bridge speaks whichever
/// dialect it was built for.
///
/// The two ecosystems disagree in small ways that would each silently break a
/// real hook. A bridge that spoke one dialect's shape at the other's point
/// would produce hooks that run and read nothing.
#[tokio::test]
async fn the_codex_dialect_writes_its_own_shape() {
    let mut f = fixture(HookDialect::Codex, vec![], 1);
    f.hooks.config.model = "a-model".into();

    f.hooks.user_prompt_submit(4, "hello").await;

    let spec = f.executor.calls().remove(0);
    let payload: serde_json::Value = serde_json::from_str(&spec.stdin).expect("json");
    assert_eq!(payload["model"], "a-model", "every Codex payload names it");
    assert_eq!(payload["turn_id"], "4", "and a turn id, as a string");
    assert!(
        !spec.stdin.ends_with('\n'),
        "Codex stdin carries no trailing newline"
    );
}

/// TC-HOOK-WATCH-3: a prompt hook's text is added to what the model reads.
#[tokio::test]
async fn a_prompt_hook_can_add_context() {
    let f = fixture(
        HookDialect::ClaudeCode,
        vec![said(
            r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"remember the style guide"}}"#,
        )],
        1,
    );

    let watched = f.hooks.user_prompt_submit(1, "write a function").await;
    let texts: Vec<String> = watched.contexts.into_iter().map(|m| m.content).collect();
    assert_eq!(texts, ["remember the style guide"]);
}

/// TC-HOOK-WATCH-4: a prompt hook may refuse the prompt, in its own words.
///
/// A person told only "blocked" cannot tell a deliberate policy from a broken
/// hook, so the refusal carries whatever the hook said.
#[tokio::test]
async fn a_prompt_hook_can_refuse_and_says_why() {
    let denied = MergedHookOutcome {
        decision: MergedDecision::Deny,
        reason: Some("secrets in the prompt".into()),
        ..Default::default()
    };
    assert_eq!(
        prompt_refusal(&denied).as_deref(),
        Some("secrets in the prompt")
    );

    // A hook that asks the turn not to proceed refuses the prompt too: both
    // mean the model never sees what was typed.
    let halted = MergedHookOutcome {
        stop: true,
        stop_reason: Some("rate limited".into()),
        ..Default::default()
    };
    assert_eq!(prompt_refusal(&halted).as_deref(), Some("rate limited"));

    // A refusal with no words still refuses, rather than silently allowing -
    // and it still says something. An empty string would reach a person as a
    // blank rejection, which reads as a bug in the harness rather than as a
    // policy somebody configured.
    let bare = MergedHookOutcome {
        decision: MergedDecision::Deny,
        ..Default::default()
    };
    let words = prompt_refusal(&bare).expect("a refusal");
    assert!(!words.trim().is_empty(), "refused with no words: {words:?}");
}

/// TC-HOOK-WATCH-5: a quiet prompt hook refuses nothing.
///
/// Silence is the common case and must be inert, or a deployment's first
/// observing hook would start rejecting prompts.
#[tokio::test]
async fn a_quiet_prompt_hook_refuses_nothing() {
    for quiet in [
        MergedDecision::None,
        MergedDecision::Allow,
        MergedDecision::Ask,
    ] {
        assert_eq!(
            prompt_refusal(&MergedHookOutcome {
                decision: quiet,
                ..Default::default()
            }),
            None,
            "{quiet:?} is not a refusal"
        );
    }
}

/// TC-HOOK-WATCH-6: at `Stop`, a blocking answer asks the turn to keep going.
///
/// The one inverted point. Everywhere else a hook's `continue: false` asks the
/// turn to halt; here the turn is already ending, so the same field is the
/// only way a hook can ask for more work. Reading it the usual way round would
/// turn "do not stop yet" into "stop", which is the exact opposite of what the
/// deployment configured.
#[tokio::test]
async fn a_stop_hook_that_blocks_asks_for_more_work() {
    let blocking = MergedHookOutcome {
        decision: MergedDecision::Deny,
        reason: Some("the tests have not been run".into()),
        ..Default::default()
    };
    assert_eq!(
        stop_veto(&blocking).as_deref(),
        Some("the tests have not been run")
    );

    let halting = MergedHookOutcome {
        stop: true,
        stop_reason: Some("still working".into()),
        ..Default::default()
    };
    assert_eq!(stop_veto(&halting).as_deref(), Some("still working"));
}

/// TC-HOOK-WATCH-7: a quiet `Stop` hook lets the turn end.
#[tokio::test]
async fn a_quiet_stop_hook_lets_the_turn_end() {
    assert_eq!(stop_veto(&MergedHookOutcome::default()), None);
    assert_eq!(
        stop_veto(&MergedHookOutcome {
            decision: MergedDecision::Allow,
            ..Default::default()
        }),
        None
    );
}

/// TC-HOOK-WATCH-8: every watch run leaves its audit pair.
///
/// Same rule as the tool points: `hook/invoked` before the run and
/// `hook/result` after, so a hook that hangs is distinguishable from one that
/// was never selected.
#[tokio::test]
async fn each_watch_run_writes_its_audit_pair() {
    let f = fixture(HookDialect::ClaudeCode, vec![], 1);
    f.hooks.stop(2).await;

    let events: Vec<(String, String)> = f
        .log
        .events()
        .into_iter()
        .map(|e| {
            (
                e.ty,
                e.data
                    .get("point")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect();
    assert_eq!(
        events,
        [
            ("hook/invoked".to_owned(), "Stop".to_owned()),
            ("hook/result".to_owned(), "Stop".to_owned()),
        ]
    );
}

/// TC-HOOK-WATCH-9: a point with nothing configured runs nothing.
///
/// A bridge installed for one point must not spawn processes for the others.
#[tokio::test]
async fn a_point_with_no_hooks_runs_nothing() {
    let mut f = fixture(HookDialect::ClaudeCode, vec![], 1);
    f.hooks.session_start = Vec::new();

    let watched = f.hooks.session_start("startup").await;

    assert!(f.executor.calls().is_empty());
    assert!(watched.contexts.is_empty());
    assert!(f.log.events().is_empty(), "and writes no audit");
}

/// TC-HOOK-WATCH-10: several hooks at one point each contribute a message.
///
/// They are different programs with different things to say, so joining their
/// notes would invent one voice for several remarks - the same rule the
/// `PostToolUse` point follows.
#[tokio::test]
async fn several_watch_hooks_each_contribute_a_message() {
    let f = fixture(
        HookDialect::ClaudeCode,
        vec![
            said(
                r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"one"}}"#,
            ),
            said(
                r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"two"}}"#,
            ),
        ],
        2,
    );

    let watched = f.hooks.user_prompt_submit(1, "go").await;
    let texts: Vec<String> = watched.contexts.into_iter().map(|m| m.content).collect();
    assert_eq!(texts, ["one", "two"]);
    assert_eq!(f.executor.calls().len(), 2);
}
