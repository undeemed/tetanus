//! Test Design Specification: upstream named regressions, ported.
//!
//! Features under test: the invariants upstream keeps pinned in
//! `packages/core/agent-loop/tests/contract-regressions.spec.ts` - the ones a
//! refactor breaks quietly. Each case names the upstream case it comes from.
//!
//! Approach: one offline turn per case over a temporary journal, driven
//! through the bus. Most of upstream's file is about surfaces tetanus has not
//! built: fiber disposal, a steering inbox, a `finish {kind:error}` chunk, and
//! containment of a throwing listener. Those cases have nothing to restate and
//! stay rows in `docs/parity.md`.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tetanus_core::EventBus;
use tetanus_session::{JsonlSessionLog, SessionEventDispatch, SessionLog};
use tetanus_turn::boot::boot;
use tetanus_turn::events::{AgentRequest, LlmStream, ToolsPostExecute};
use tetanus_turn::llm::mock::MockAdapter;
use tetanus_turn::log::topic;
use tetanus_turn::tools::{EchoTool, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine};

/// TC-PORT-REG-1: an event is on the log before its listener is told about it.
///
/// Upstream: "the step/start event is in session.events when its session/event
/// listener fires".
///
/// This is the commit-point rule seen from the other side: a listener that
/// reads the log must never see a journal that is missing the very event it
/// was woken for, or instrumentation reports a state that never existed.
///
/// Expected: at every `step/start` dispatch, the log's last event is that
/// `step/start`, and its `seq` matches the dispatched event's.
#[tokio::test]
async fn a_listener_sees_the_log_that_already_holds_its_event() {
    let f = Fixture::new("reg-publication-order").await;

    let log = Arc::clone(&f.log);
    let mismatches: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let found = Arc::clone(&mismatches);
    let _watch = f.bus.on_emit::<SessionEventDispatch>(move |ev| {
        if ev.event.ty != topic::STEP_START {
            return;
        }
        let events = log.events();
        let last = events.last().expect("the log is not empty");
        if last.ty != ev.event.ty || last.seq != ev.event.seq {
            found
                .lock()
                .expect("found")
                .push(format!("last was {} seq {}", last.ty, last.seq));
        }
    });

    f.engine.run_turn("publication order").await.expect("turn");

    assert!(
        mismatches.lock().expect("found").is_empty(),
        "a listener saw a log missing its own event: {:?}",
        mismatches.lock().expect("found")
    );
    assert_eq!(
        f.log
            .events()
            .iter()
            .filter(|e| e.ty == topic::STEP_START)
            .count(),
        2,
        "the case did observe the two steps of a mock turn"
    );
}

/// TC-PORT-REG-2: a tool result is filed under the model's call id, whatever a
/// post-execute listener does.
///
/// Upstream: "the loop records tool/result under the model call.id even when a
/// post-execute listener replaces content".
///
/// The call id is the model's, and it is what the next request joins the result
/// to. A listener owns the outcome, never the identity of the call it answers.
///
/// Input: a `tools/post-execute` listener that replaces the content and also
/// rewrites the call it was handed.
/// Expected: the logged `tool/result` carries the replaced content under the
/// original id and name, citing the `tool/call` it answers.
#[tokio::test]
async fn a_replaced_result_keeps_the_models_call_id() {
    let f = Fixture::new("reg-call-identity").await;

    let _rewrite = f.bus.on_waterfall::<ToolsPostExecute, _>(|ev, next| {
        ev.outcome.content = "replaced by a listener".into();
        ev.call.id = "not-the-model's-id".into();
        ev.call.name = "not-the-model's-tool".into();
        Box::pin(next.run(ev))
    });

    f.engine.run_turn("call identity").await.expect("turn");

    let events = f.log.events();
    let call = events
        .iter()
        .find(|e| e.ty == topic::TOOL_CALL)
        .expect("a tool call");
    let result = events
        .iter()
        .find(|e| e.ty == topic::TOOL_RESULT)
        .expect("a tool result");

    assert_eq!(result.data["call_id"], call.data["id"]);
    assert_eq!(result.data["name"], call.data["name"]);
    assert_eq!(result.data["content"], "replaced by a listener");
    assert_eq!(
        result.source_event_seqs.as_deref(),
        Some(&[call.seq][..]),
        "the result still cites the call it answers"
    );
}

/// TC-PORT-REG-3: what `agent/request` decides is what the provider is called
/// with.
///
/// Upstream: "the agent/request waterfall can supply the model for a
/// model-less agent" - the same rule, that routing is the waterfall's output
/// and not the value the driver started from.
///
/// Input: a listener that rewrites the request's model on the first step only.
/// Expected: the first provider call sees the rewritten model, the second sees
/// the session's own, so the rewrite is per-request and is not sticky state.
#[tokio::test]
async fn the_request_waterfall_decides_what_the_provider_is_called_with() {
    let f = Fixture::new("reg-request-routing").await;

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _record = f.bus.on_waterfall::<LlmStream, _>(move |ev, next| {
        sink.lock().expect("seen").push(ev.request.model.clone());
        Box::pin(next.run(ev))
    });

    let calls = Arc::new(Mutex::new(0u32));
    let counted = Arc::clone(&calls);
    let _route = f.bus.on_waterfall::<AgentRequest, _>(move |ev, next| {
        let mut n = counted.lock().expect("calls");
        *n += 1;
        if *n == 1 {
            ev.request.model = "routed-elsewhere".into();
        }
        drop(n);
        Box::pin(next.run(ev))
    });

    f.engine.run_turn("routing").await.expect("turn");

    let seen = seen.lock().expect("seen").clone();
    let default = TurnConfig::default().model;
    assert_eq!(seen, vec!["routed-elsewhere".to_string(), default]);
}

/// One booted turn engine over a fresh journal, with the log kept so a case
/// can read it back. Deliberately local: the shared turn-flow fixture does not
/// publish its log, and this suite reads the log in two of its three cases.
struct Fixture {
    engine: TurnEngine,
    log: Arc<JsonlSessionLog>,
    bus: EventBus,
    _dir: TempDir,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(format!("{name}.jsonl"));
        let bus = EventBus::new();
        let concrete = JsonlSessionLog::create("regressions", &path, bus.clone()).expect("journal");
        let log: Arc<dyn SessionLog> = Arc::clone(&concrete) as Arc<dyn SessionLog>;
        let ctx = boot(
            bus.clone(),
            Arc::new(MockAdapter::new()),
            Arc::new(ToolRegistry::new().with(Arc::new(EchoTool))),
            log,
        )
        .expect("boot");
        let engine = TurnEngine::from_context(&ctx, TurnConfig::default()).expect("engine");
        Self {
            engine,
            log: concrete,
            bus,
            _dir: dir,
        }
    }
}
