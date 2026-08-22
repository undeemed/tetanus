//! Test Design Specification: the permission gate on the tool pipeline,
//! ported.
//!
//! Feature under test: the decision one tool call is gated on, as the turn
//! engine applies it - which calls are put to an answerer, what a grant runs,
//! what a denial writes, and what the model is told. Upstream pins the seam in
//! `packages/interaction/user-approval/tests/approval.spec.ts` and its use in
//! `packages/core/agent-loop`; `crates/turn/tests/upstream_approval.rs` already
//! ports the seam itself, so what is restated here is the wiring: the gate in
//! the pipeline, and the promise that a refused call never runs.
//!
//! Approach: a real turn through the shared harness, with a tool that gates
//! itself and counts its own executions. Counting is the load-bearing part: a
//! case that only read the `tool/result` could not tell a refused call from one
//! that ran and reported a refusal, and those are very different programs.
//!
//! What is not restated, and why. Upstream's scoped routing - a UI that answers
//! only for the agents it owns - has no counterpart, because tetanus has one
//! bus per session; `upstream_approval.rs` records that. Its `ui/approve` wire
//! frame belongs to the presentation lane by `docs/interface-contract.md` §5,
//! so what is asserted here is the engine half: the ask, the outcome, and the
//! journal.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime.
//! One case panics on purpose and drops exactly its payload from the hook.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic that escapes the seam.

mod harness;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};

use harness::Harness;
use serde_json::Value;
use tetanus_core::EventBus;
use tetanus_session::SessionEvent;
use tetanus_turn::approval::{
    ApprovalAsk, ApprovalOutcome, ApprovalPolicy, ApprovalService, TOOL_NOT_PERMITTED,
};
use tetanus_turn::log::topic;
use tetanus_turn::tools::{
    Permission, Tool, ToolError, ToolMode, ToolOutcome, ToolRegistry, ToolSchema,
};
use tetanus_turn::{StopReason, TurnConfig};

/// The tool the mock adapter calls, gated and counting.
///
/// It is named `echo` because the offline adapter asks for `echo`; what it does
/// is beside the point, and what matters is that it demands a decision and
/// records whether its body ever ran.
struct GatedEcho {
    ran: Arc<AtomicUsize>,
    reason: &'static str,
}

impl GatedEcho {
    fn new(reason: &'static str) -> (Arc<Self>, Arc<AtomicUsize>) {
        let ran = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                ran: Arc::clone(&ran),
                reason,
            }),
            ran,
        )
    }
}

#[async_trait::async_trait]
impl Tool for GatedEcho {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "echo".into(),
            description: "Return the given text unchanged.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
            }),
        }
    }

    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    fn permission(&self, _arguments: &Value) -> Permission {
        Permission::ask(self.reason)
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutcome::ok(
            arguments["text"].as_str().unwrap_or_default(),
        ))
    }
}

async fn gated(name: &str, reason: &'static str) -> (Harness, Arc<AtomicUsize>) {
    let (tool, ran) = GatedEcho::new(reason);
    let harness = Harness::with_tools(name, ToolRegistry::new().with(tool)).await;
    (harness, ran)
}

/// Answer every ask with one outcome, and count the questions.
fn answerer(
    bus: &EventBus,
    outcome: ApprovalOutcome,
) -> (tetanus_core::EffectHandle, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    let handle = bus.on_waterfall::<ApprovalAsk, _>(move |_ev, _next| {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { outcome })
    });
    (handle, asked)
}

/// One question, as the answerer saw it.
#[derive(Debug, Clone)]
struct Question {
    tool_name: String,
    call_id: Option<String>,
    reason: Option<String>,
    /// The id the audit pair is written under.
    ask_id: String,
}

/// What an answerer recorded, shared with the case that reads it back.
type Asked = Arc<std::sync::Mutex<Vec<Question>>>;

fn of_type<'a>(events: &'a [SessionEvent], ty: &str) -> Vec<&'a SessionEvent> {
    events.iter().filter(|event| event.ty == ty).collect()
}

/// The durable sequence, as topics, from the first `tool/call` onwards.
fn tail_from_tool_call(events: &[SessionEvent]) -> Vec<String> {
    let start = events
        .iter()
        .position(|event| event.ty == topic::TOOL_CALL)
        .expect("the turn made a tool call");
    events[start..]
        .iter()
        .take(4)
        .map(|event| event.ty.clone())
        .collect()
}

/// TC-PORT-INT-1: a granted call runs, and the journal reads decision then
/// effect.
///
/// Upstream: `allowed-once` is the one grant, and it runs the call it was asked
/// about.
///
/// Input: a turn whose tool gates itself, with an answerer that grants.
/// Expected: the body ran once; the journal reads `tool/call`,
/// `approval/asked`, `approval/decided`, `tool/result`, in that order; the
/// result carries no `code`, because a call that ran has an outcome to report.
#[tokio::test]
async fn a_granted_call_runs_and_the_journal_reads_decision_then_effect() {
    let (h, ran) = gated("granted", "echo the text back").await;
    let (_answerer, asked) = answerer(h.bus(), ApprovalOutcome::AllowedOnce);

    let outcome = h.engine.run_turn("hello").await.expect("turn");

    assert_eq!(ran.load(Ordering::SeqCst), 1, "the granted call ran");
    assert_eq!(asked.load(Ordering::SeqCst), 1, "it was asked about once");
    assert_eq!(outcome.reason, StopReason::Natural);
    let events = h.engine.log().events();
    assert_eq!(
        tail_from_tool_call(&events),
        [
            topic::TOOL_CALL,
            topic::APPROVAL_ASKED,
            topic::APPROVAL_DECIDED,
            topic::TOOL_RESULT
        ]
    );
    let result = of_type(&events, topic::TOOL_RESULT)[0];
    assert_eq!(result.data["ok"], true);
    assert!(
        result.data.get("code").is_none(),
        "a call that ran reports an outcome, not a code: {}",
        result.data
    );
}

/// TC-PORT-INT-2: a refused call never runs, and the model is told why.
///
/// Upstream: "a refused call never runs and the model is told why", pinned in
/// its interception spec and restated here for the permission gate.
///
/// Input: the same turn with an answerer that rejects.
/// Expected: the body never ran; the `tool/result` is `ok: false` carrying
/// `TOOL_NOT_PERMITTED` and a sentence naming the tool and saying not to retry;
/// the turn still ends naturally. A denial is the seam working, so it is a
/// result the model reads and not a failure of the turn.
#[tokio::test]
async fn a_refused_call_never_runs_and_the_result_says_why() {
    let (h, ran) = gated("refused", "echo the text back").await;
    let (_answerer, _asked) = answerer(h.bus(), ApprovalOutcome::Rejected);

    let outcome = h.engine.run_turn("hello").await.expect("turn");

    assert_eq!(ran.load(Ordering::SeqCst), 0, "the refused call never ran");
    assert_eq!(outcome.reason, StopReason::Natural);
    let events = h.engine.log().events();
    let result = of_type(&events, topic::TOOL_RESULT)[0];
    assert_eq!(result.data["ok"], false);
    assert_eq!(result.data["code"], TOOL_NOT_PERMITTED);
    let content = result.data["content"].as_str().expect("content");
    assert!(content.contains("`echo`"), "{content}");
    assert!(content.contains("Do not retry"), "{content}");
    assert_eq!(
        of_type(&events, topic::APPROVAL_DECIDED)[0].data["outcome"],
        "rejected"
    );
}

/// TC-PORT-INT-3: with nobody answering, the run neither hangs nor grants.
///
/// Upstream: the seam fails closed on every way of not getting an answer.
///
/// Input: the same turn with no answerer registered at all - the headless
/// default.
/// Expected: the turn completes; the outcome is `unavailable`; the call never
/// ran; and the journal carries the pair, so a reader of an unattended run can
/// see what was asked and that nobody was there. This is the case a gate gets
/// wrong by waiting forever, and the test would hang rather than fail if it
/// did.
#[tokio::test]
async fn with_no_answerer_the_headless_default_denies_and_records_it() {
    let (h, ran) = gated("headless", "echo the text back").await;

    let outcome = h.engine.run_turn("hello").await.expect("turn");

    assert_eq!(outcome.reason, StopReason::Natural);
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    let events = h.engine.log().events();
    assert_eq!(
        of_type(&events, topic::APPROVAL_DECIDED)[0].data["outcome"],
        "unavailable"
    );
    let content = of_type(&events, topic::TOOL_RESULT)[0].data["content"]
        .as_str()
        .expect("content")
        .to_string();
    assert!(content.contains("unattended"), "{content}");
}

/// TC-PORT-INT-4: under `never`, nobody is asked and the answer is a decision.
///
/// Upstream: "'never' auto-rejects every ask without dispatching", pinned by
/// its prepend cases - a policy applied as a listener could be answered ahead
/// of by a listener registered later.
///
/// Input: the same turn under the `never` policy, with an answerer that would
/// have granted.
/// Expected: the answerer is never consulted; the outcome is `rejected`, not
/// `unavailable`, because a deployment that chose `never` did decide; the call
/// never ran.
#[tokio::test]
async fn under_never_no_answerer_is_consulted_and_the_denial_is_a_decision() {
    let (tool, ran) = GatedEcho::new("echo the text back");
    let h = Harness::with_config(
        "never",
        ToolRegistry::new().with(tool),
        TurnConfig {
            approval_policy: ApprovalPolicy::Never,
            ..TurnConfig::default()
        },
    )
    .await;
    let (_answerer, asked) = answerer(h.bus(), ApprovalOutcome::AllowedOnce);

    h.engine.run_turn("hello").await.expect("turn");

    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "a `never` policy puts the question to nobody"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    let events = h.engine.log().events();
    assert_eq!(
        of_type(&events, topic::APPROVAL_DECIDED)[0].data["outcome"],
        "rejected"
    );
}

/// TC-PORT-INT-5: the ask names the tool, the call and the reason.
///
/// Upstream: `ui/approve` carries `tool_name`, `call_id` and `reason`, and
/// `request_id` is the audit line's id.
///
/// Input: a turn with an answerer that records what it was asked.
/// Expected: the tool name, the `tool/call.id` the step streamed, and the
/// tool's own words; and the same id on both halves of the audit pair. Without
/// the call id a surface cannot show which call it is prompting about when two
/// are in flight.
#[tokio::test]
async fn the_ask_names_the_tool_the_call_and_the_reason() {
    let (h, _ran) = gated("named", "echo the text back; this is a test gate").await;
    let seen: Asked = Arc::default();
    let recorder = Arc::clone(&seen);
    let _answerer = h.bus().on_waterfall::<ApprovalAsk, _>(move |ev, _next| {
        recorder.lock().expect("seen").push(Question {
            tool_name: ev.request.tool_name.clone(),
            call_id: ev.request.call_id.clone(),
            reason: ev.request.reason.clone(),
            ask_id: ev.id.clone(),
        });
        Box::pin(async move { ApprovalOutcome::AllowedOnce })
    });

    h.engine.run_turn("hello").await.expect("turn");

    let seen = seen.lock().expect("seen").clone();
    assert_eq!(seen.len(), 1);
    let question = &seen[0];
    assert_eq!(question.tool_name, "echo");
    assert_eq!(question.call_id.as_deref(), Some("call_1"));
    assert_eq!(
        question.reason.as_deref(),
        Some("echo the text back; this is a test gate")
    );
    let events = h.engine.log().events();
    assert_eq!(
        of_type(&events, topic::APPROVAL_ASKED)[0].data["id"],
        question.ask_id
    );
    assert_eq!(
        of_type(&events, topic::APPROVAL_DECIDED)[0].data["id"],
        question.ask_id,
        "the pair shares one id"
    );
}

/// TC-PORT-INT-6: a grant is for one call, and the next one asks again.
///
/// Upstream: "a grant is for the one call it was asked about. It is not a rule,
/// not a session setting and not a grant for the same tool later".
///
/// Input: two turns on one session, both granted.
/// Expected: two questions, two audit pairs with different ids, and two runs. A
/// gate that remembered the first grant would be a permission system nobody
/// asked for.
#[tokio::test]
async fn a_grant_covers_one_call_and_the_next_call_asks_again() {
    let (h, ran) = gated("twice", "echo the text back").await;
    let (_answerer, asked) = answerer(h.bus(), ApprovalOutcome::AllowedOnce);

    h.engine.run_turn("first").await.expect("first turn");
    h.engine.run_turn("second").await.expect("second turn");

    assert_eq!(asked.load(Ordering::SeqCst), 2);
    assert_eq!(ran.load(Ordering::SeqCst), 2);
    let events = h.engine.log().events();
    let ids: Vec<&Value> = of_type(&events, topic::APPROVAL_ASKED)
        .iter()
        .map(|event| &event.data["id"])
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "an id is fresh per ask and never reused");
}

/// TC-PORT-INT-7: a tool that gates nothing writes no audit at all.
///
/// Upstream: only the tools that ask are asked about.
///
/// Input: the ordinary harness, whose `echo` tool declares no permission.
/// Expected: the tool ran, and the journal holds no `approval/*` event. A gate
/// that wrote a pair per call would drown the audit that matters, and every
/// existing conformance sequence would have changed shape.
#[tokio::test]
async fn an_ungated_tool_leaves_the_journal_exactly_as_it_was() {
    let h = Harness::new("ungated").await;

    h.engine.run_turn("hello").await.expect("turn");

    let events = h.engine.log().events();
    assert!(of_type(&events, topic::APPROVAL_ASKED).is_empty());
    assert!(of_type(&events, topic::APPROVAL_DECIDED).is_empty());
    assert_eq!(of_type(&events, topic::TOOL_RESULT)[0].data["ok"], true);
}

/// TC-PORT-INT-8: the journal on disk replays the same decision sequence.
///
/// Upstream: the log is the record, and a replay derives from it alone.
///
/// Input: a turn whose gated call was refused, replayed from the file.
/// Expected: the same four records in the same order, with the same outcome and
/// the same result - and the result citing the `tool/call` it answers, so the
/// pairing survives a read that starts mid-turn. A decision a replay cannot
/// show is a decision nobody can audit.
#[tokio::test]
async fn a_replay_from_disk_shows_what_was_asked_and_what_was_answered() {
    let (h, _ran) = gated("replayed", "echo the text back").await;
    let (_answerer, _asked) = answerer(h.bus(), ApprovalOutcome::Rejected);
    h.engine.run_turn("hello").await.expect("turn");
    h.engine.flush().await.expect("flush");

    let replayed = tetanus_session::replay(&h.log_path).expect("replay");

    assert_eq!(
        tail_from_tool_call(&replayed),
        [
            topic::TOOL_CALL,
            topic::APPROVAL_ASKED,
            topic::APPROVAL_DECIDED,
            topic::TOOL_RESULT
        ]
    );
    let call = of_type(&replayed, topic::TOOL_CALL)[0];
    let result = of_type(&replayed, topic::TOOL_RESULT)[0];
    assert_eq!(
        of_type(&replayed, topic::APPROVAL_DECIDED)[0].data["outcome"],
        "rejected"
    );
    assert_eq!(result.data["code"], TOOL_NOT_PERMITTED);
    assert_eq!(
        result.source_event_seqs,
        Some(vec![call.seq]),
        "the result cites the call it answers"
    );
}

/// TC-PORT-INT-9: a denied call is not on the derived history as an outcome
/// the model can misread.
///
/// Upstream: the model is told, so it can do something else.
///
/// Input: a refused turn's log, derived into model messages.
/// Expected: the tool message the model reads is the refusal text, and the
/// audit events contribute nothing - a model that saw `approval/decided` would
/// be reading the harness's own bookkeeping as conversation.
#[tokio::test]
async fn what_the_model_reads_is_the_refusal_and_not_the_audit() {
    let (h, _ran) = gated("history", "echo the text back").await;
    let (_answerer, _asked) = answerer(h.bus(), ApprovalOutcome::Rejected);
    h.engine.run_turn("hello").await.expect("turn");

    let messages = tetanus_turn::log::derive_messages(&h.engine.log().events());

    let tool_messages: Vec<&tetanus_turn::llm::Message> = messages
        .iter()
        .filter(|m| m.role == tetanus_turn::llm::Role::Tool)
        .collect();
    assert_eq!(tool_messages.len(), 1);
    assert!(
        tool_messages[0].content.contains("was not permitted"),
        "{}",
        tool_messages[0].content
    );
    assert!(
        !messages.iter().any(|m| m.content.contains("rejected")),
        "the audit is not conversation"
    );
}

/// TC-PORT-INT-10: a permission classifier that panics asks rather than runs.
///
/// Upstream: a throwing classifier is contained.
///
/// Input: a tool whose `permission` panics, in a run with no answerer.
/// Expected: the call is treated as needing a decision, nobody answers, and it
/// does not run. The direction is the opposite of the scheduling classifier's,
/// and deliberately so: for scheduling the safe answer is "overlap nothing",
/// and for permission it is "ask". The cost of being wrong here is a question;
/// the cost of the other direction is an irreversible call nobody approved.
#[tokio::test]
async fn a_panicking_permission_classifier_fails_closed() {
    struct Faulty(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Tool for Faulty {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "echo".into(),
                description: "Return the given text unchanged.".into(),
                parameters: serde_json::json!({ "type": "object" }),
            }
        }
        fn permission(&self, _arguments: &Value) -> Permission {
            panic!("{DELIBERATE}");
        }
        async fn execute(&self, _arguments: &Value) -> Result<ToolOutcome, ToolError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutcome::ok("ran"))
        }
    }
    quieten_deliberate_panics();
    let ran = Arc::new(AtomicUsize::new(0));
    let h = Harness::with_tools(
        "panicking-permission",
        ToolRegistry::new().with(Arc::new(Faulty(Arc::clone(&ran)))),
    )
    .await;

    h.engine.run_turn("hello").await.expect("turn");

    assert_eq!(ran.load(Ordering::SeqCst), 0, "it was not run unasked");
    let events = h.engine.log().events();
    let asked = of_type(&events, topic::APPROVAL_ASKED);
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].data["reason"]
            .as_str()
            .expect("reason")
            .contains("panicked"),
        "whoever answers is told why they are being asked: {}",
        asked[0].data
    );
}

/// TC-PORT-INT-11: the deployment default is what a journal with no switch
/// runs under, and a switch on the journal beats it.
///
/// Upstream: the policy is the last `approval/policy` on the journal, else the
/// deployment's setting.
///
/// Input: an engine whose configured default is `never`, switched to `ask` on
/// its own journal.
/// Expected: the engine's approval service reports `ask` afterwards, and the
/// gated call is put to the answerer that grants it. The gate and a surface
/// reading `approval.set` must consult one service, or the policy a caller set
/// is not the policy the gate read.
#[tokio::test]
async fn a_session_switch_beats_the_deployment_default_at_the_gate() {
    let (tool, ran) = GatedEcho::new("echo the text back");
    let h = Harness::with_config(
        "switched",
        ToolRegistry::new().with(tool),
        TurnConfig {
            approval_policy: ApprovalPolicy::Never,
            ..TurnConfig::default()
        },
    )
    .await;
    let (_answerer, asked) = answerer(h.bus(), ApprovalOutcome::AllowedOnce);
    let approvals: &Arc<ApprovalService> = h.engine.approvals();

    assert_eq!(approvals.policy(), ApprovalPolicy::Never);
    assert!(approvals.set_policy(ApprovalPolicy::Ask).expect("switch"));
    h.engine.run_turn("hello").await.expect("turn");

    assert_eq!(approvals.policy(), ApprovalPolicy::Ask);
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert_eq!(ran.load(Ordering::SeqCst), 1);
}

/// The payload the deliberate panic carries, so the hook drops exactly it.
const DELIBERATE: &str = "deliberate: a permission classifier with a bug";

/// Keep the deliberate panic out of the test output without hiding any other.
fn quieten_deliberate_panics() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let payload = info.payload();
            let deliberate = payload
                .downcast_ref::<&str>()
                .is_some_and(|text| text.contains(DELIBERATE));
            if !deliberate {
                previous(info);
            }
        }));
    });
}
