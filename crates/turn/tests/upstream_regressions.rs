//! Test Design Specification: upstream named regressions, ported.
//!
//! Features under test: the invariants upstream keeps pinned in
//! `packages/core/agent-loop/tests/contract-regressions.spec.ts` - the ones a
//! refactor breaks quietly. Each case names the upstream case it comes from.
//!
//! Approach: one offline turn per case over a temporary journal, driven
//! through the bus. Most of upstream's file is about surfaces tetanus has not
//! built: fiber disposal, a steering inbox, and a `finish {kind:error}` chunk.
//! Those cases have nothing to restate and stay rows in `docs/parity.md`.
//! Containment of a throwing listener is restated at bus level in
//! `crates/core/tests/containment.rs` and at turn level below.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tetanus_core::EventBus;
use tetanus_session::{JsonlSessionLog, SessionEventDispatch, SessionLog};
use tetanus_turn::boot::boot;
use tetanus_turn::events::{AgentRequest, AssemblePrompt, LlmStream, ToolsPostExecute};
use tetanus_turn::llm::mock::MockAdapter;
use tetanus_turn::log::topic;
use tetanus_turn::tools::{EchoTool, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine, TurnError};

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

/// TC-PORT-REG-4: a panicking `session/event` observer cannot change a turn.
///
/// Upstream: "a throwing session/event listener on turn/end is contained (turn
/// still balanced, loop survives)" and "a throwing step/start observer cannot
/// change a successful turn".
///
/// Input: an observer that panics on every durable event, registered before a
/// second observer that records what it sees.
/// Expected: the turn finishes naturally with its answer, the journal holds
/// the same events it holds without the panicking observer, and the observer
/// behind it saw every one of them. Instrumentation with a bug is the
/// instrumentation's problem, not the turn's.
#[tokio::test]
async fn a_panicking_session_event_observer_cannot_change_a_turn() {
    let quiet = Fixture::new("reg-containment-quiet").await;
    let expected = quiet.engine.run_turn("containment").await.expect("turn");
    let expected_log: Vec<String> = quiet.log.events().iter().map(|e| e.ty.clone()).collect();

    let f = Fixture::new("reg-containment").await;
    let _bug = f
        .bus
        .on_emit::<SessionEventDispatch>(|_| panic!("an observer with a bug"));
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let peer = Arc::clone(&seen);
    let _watch = f.bus.on_emit::<SessionEventDispatch>(move |ev| {
        peer.lock().expect("seen").push(ev.event.ty.clone());
    });

    let outcome = f.engine.run_turn("containment").await.expect("turn");

    assert_eq!(outcome.reason, expected.reason);
    assert_eq!(outcome.steps, expected.steps);
    assert_eq!(outcome.content, expected.content);
    let written: Vec<String> = f.log.events().iter().map(|e| e.ty.clone()).collect();
    assert_eq!(written, expected_log, "the journal is what it always was");
    assert_eq!(
        seen.lock().expect("seen").clone(),
        written,
        "the observer behind the panicking one saw every event"
    );
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

/// TC-PORT-REG-5: a decision listener that panics ends its turn, and closes
/// the journal behind it.
///
/// Upstream: "plugin exceptions are contained" - the half about a *decision*
/// listener rather than an observer, which TC-PORT-REG-4 covers.
///
/// The bus keeps `serial` and `waterfall` loud by design, and that does not
/// change here: a listener that decides something is not instrumentation, and
/// a caller that asked one question should hear that it could not be answered.
/// What is contained is the blast radius. Before this the panic escaped
/// `run_turn` entirely, so `turn/start` sat unbalanced on the journal and the
/// session was wedged open: a reader could not tell the turn was over, and the
/// next `session.create` had to synthesize `interrupted` closers for a turn
/// nothing had interrupted.
///
/// Input: a `system-prompt/assemble` listener that panics, so the turn dies
/// inside a step that has already opened.
/// Expected: `run_turn` returns `TurnError::Plugin` carrying the panic message
/// rather than unwinding; the journal is balanced, ending `step/end` then
/// `turn/end`; and `turn/end` reads `stop_reason: "failed"`, which is what
/// contract section 4.4.2 says every failed turn reads.
#[tokio::test]
async fn a_panicking_decision_listener_ends_the_turn_and_balances_the_journal() {
    quiet_deliberate_panics();
    let f = Fixture::new("reg-decision-panic").await;
    let _bug = f
        .bus
        .on_waterfall::<AssemblePrompt, _>(|_ev, _next| panic!("{DELIBERATE}"));

    let failed = f
        .engine
        .run_turn("a listener with a bug")
        .await
        .expect_err("a panicking decision listener fails its turn");

    match &failed {
        TurnError::Plugin(fault) => assert!(
            fault.contains(DELIBERATE),
            "the panic's own message is what the caller is told: {fault}"
        ),
        other => panic!("expected a contained plugin panic, got {other:?}"),
    }

    let written: Vec<String> = f.log.events().iter().map(|e| e.ty.clone()).collect();
    assert_eq!(
        written.last().map(String::as_str),
        Some(topic::TURN_END),
        "the turn is closed on the journal: {written:?}"
    );
    assert_eq!(
        written.iter().filter(|ty| *ty == topic::TURN_START).count(),
        written.iter().filter(|ty| *ty == topic::TURN_END).count(),
        "every turn opened is a turn ended: {written:?}"
    );
    assert_eq!(
        written.iter().filter(|ty| *ty == topic::STEP_START).count(),
        written.iter().filter(|ty| *ty == topic::STEP_END).count(),
        "and the step the panic interrupted is ended too: {written:?}"
    );

    let ended = f
        .log
        .events()
        .into_iter()
        .find(|e| e.ty == topic::TURN_END)
        .expect("turn/end");
    assert_eq!(ended.data["stop_reason"], serde_json::json!("failed"));
}

/// TC-PORT-REG-6: the turn after a contained panic is an ordinary turn.
///
/// A containment that left the engine unusable would trade one wedged session
/// for another. The step counter, the turn counter and the journal all have to
/// carry on from where the failed turn left them, or a caller's only recovery
/// from a plugin bug is a new process.
///
/// Input: a turn killed by a panicking listener, the listener then removed,
/// then a second turn on the same engine and the same journal.
/// Expected: the second turn succeeds, is numbered two, and its own boundaries
/// are on the same journal after the first turn's closers - so the numbering
/// never repeats and never skips.
#[tokio::test]
async fn the_turn_after_a_contained_panic_is_an_ordinary_turn() {
    quiet_deliberate_panics();
    let f = Fixture::new("reg-decision-panic-recovery").await;

    let bug = f
        .bus
        .on_waterfall::<AssemblePrompt, _>(|_ev, _next| panic!("{DELIBERATE}"));
    f.engine
        .run_turn("the turn that dies")
        .await
        .expect_err("contained");
    drop(bug);

    let outcome = f
        .engine
        .run_turn("the turn that works")
        .await
        .expect("the engine still works");

    assert_eq!(
        outcome.turn, 2,
        "numbering carries on rather than repeating"
    );
    let turns: Vec<u64> = f
        .log
        .events()
        .iter()
        .filter(|e| e.ty == topic::TURN_START)
        .map(|e| e.data["turn"].as_u64().expect("turn"))
        .collect();
    assert_eq!(turns, vec![1, 2], "one journal, two turns, no gap");

    let written: Vec<String> = f.log.events().iter().map(|e| e.ty.clone()).collect();
    assert_eq!(
        written.iter().filter(|ty| *ty == topic::TURN_START).count(),
        written.iter().filter(|ty| *ty == topic::TURN_END).count(),
        "both turns are closed: {written:?}"
    );
}

const DELIBERATE: &str = "deliberate: a decision listener with a bug";

static QUIET: std::sync::Once = std::sync::Once::new();

/// Drop the panic report for exactly the payload these cases panic with, and
/// pass every other panic - a failed assertion, a real bug - straight through.
fn quiet_deliberate_panics() {
    QUIET.call_once(|| {
        let inherited = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let ours = info
                .payload()
                .downcast_ref::<&str>()
                .is_some_and(|message| *message == DELIBERATE);
            if !ours {
                inherited(info);
            }
        }));
    });
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
