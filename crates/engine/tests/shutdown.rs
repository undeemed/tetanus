//! Test Design Specification: what a stopping server owes its journals.
//!
//! Feature under test: `HarnessEngine::drain`, and the `"shutdown"` stop
//! reason it produces. Contract section 4.4.11 fixes all of it - a stopping
//! server interrupts every running turn at the next step boundary through the
//! mechanism `agent.interrupt` already uses, waits for them to close, and the
//! journal says `"shutdown"` rather than `"cancelled"`.
//!
//! Until this suite existed the reason was published and unproduced: §4.4.11
//! is a section, TC-PROTO-65 and -66 assert the word at the boundary, and both
//! build the value by hand. No journal tetanus wrote ever carried one. It is
//! the third stop reason found that way, after `"timed-out"` and `"repeated"`.
//!
//! Approach: the same gated adapter the cancellation suite uses, so a case
//! drains a turn that is genuinely parked inside a provider call rather than a
//! simulation of one. The distinction that matters most - shutdown is not
//! cancellation - is asserted on the journal and on the summary, because those
//! are the two places a reader meets it.
//!
//! Features NOT tested here: which signal a deployment drains on, and the
//! carrier that stops accepting connections. Both are `crates/cli`, which the
//! contract's file-ownership table gives to the presentation lane.
//!
//! Environmental needs: a temporary directory. No network, no key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::sync::Notify;

use tetanus_engine::agent::Providers;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    AgentPromptParams, Engine, SessionCreateParams, SessionEventsParams,
};
use tetanus_protocol::types::StopReason;
use tetanus_turn::llm::{mock, ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse};

/// TC-SHUTDOWN-1: a drained turn is a closed turn, and the journal says
/// `"shutdown"`.
///
/// The whole point of section 4.4.11: a server that exits cleanly leaves
/// nothing for crash repair to synthesize, so a restart is not preceded by a
/// wave of closers on every session that happened to be busy.
///
/// Input: a turn parked inside its first provider call, then a drain with a
/// generous budget.
/// Expected: the drain reports nothing left open; the prompt answers a summary
/// rather than an error; `turn/end` carries `"shutdown"`; the interrupted step
/// got its `step/end` and `turn/end` is the last event.
#[tokio::test]
async fn a_drained_turn_is_a_closed_turn() {
    let gate = Gate::new();
    let (engine, _dir) = engine(Arc::clone(&gate));
    let id = new_session(&engine).await;

    let (result, left_open) =
        tokio::join!(async { prompt(&engine, &id, "drain me").await }, async {
            gate.entered.notified().await;
            let outstanding = engine.drain(Duration::from_secs(5));
            gate.release.notify_one();
            outstanding.await
        });

    assert_eq!(left_open, 0, "the drain waited for the turn it interrupted");
    assert_eq!(
        result.summary.stop_reason,
        StopReason::Other("shutdown".into()),
        "a drained turn answers a summary, and says which stop it was"
    );

    let types = event_types(&engine, &id).await;
    assert_eq!(
        types.iter().filter(|t| *t == "step/start").count(),
        types.iter().filter(|t| *t == "step/end").count(),
        "the interrupted step was closed: {types:?}"
    );
    assert_eq!(
        types.last().map(String::as_str),
        Some("turn/end"),
        "and the turn was: {types:?}"
    );

    let reasons = stop_reasons(&engine, &id).await;
    assert_eq!(reasons, ["shutdown"], "the journal carries the word");
}

/// TC-SHUTDOWN-2: shutdown is not cancellation, on the journal.
///
/// The distinction the section spends a paragraph on. They are the same event
/// to the engine and different facts to a reader: someone pressed stop, or a
/// deployment restarted underneath the turn. A transcript that says
/// `"cancelled"` for a rolling restart sends its reader after a user who did
/// nothing.
///
/// Input: two sessions, one interrupted by a caller and one drained.
/// Expected: `"cancelled"` on the first journal and `"shutdown"` on the
/// second, and the two words are different.
#[tokio::test]
async fn shutdown_is_not_cancellation() {
    // One gate parks one call, so the two halves get an engine each rather
    // than sharing a gate that has already been spent.
    let cancel_gate = Gate::new();
    let (cancelling, _cancel_dir) = engine(Arc::clone(&cancel_gate));
    let cancelled_id = new_session(&cancelling).await;
    let (_cancelled, ()) = tokio::join!(
        async { prompt(&cancelling, &cancelled_id, "cancel me").await },
        async {
            cancel_gate.entered.notified().await;
            cancelling
                .agent_interrupt(tetanus_protocol::methods::SessionRef {
                    session_id: cancelled_id.clone(),
                })
                .await
                .expect("interrupt");
            cancel_gate.release.notify_one();
        }
    );

    let drain_gate = Gate::new();
    let (draining, _drain_dir) = engine(Arc::clone(&drain_gate));
    let drained_id = new_session(&draining).await;
    let (_drained, _open) = tokio::join!(
        async { prompt(&draining, &drained_id, "drain me").await },
        async {
            drain_gate.entered.notified().await;
            let outstanding = draining.drain(Duration::from_secs(5));
            drain_gate.release.notify_one();
            outstanding.await
        }
    );

    assert_eq!(
        stop_reasons(&cancelling, &cancelled_id).await,
        ["cancelled"]
    );
    assert_eq!(stop_reasons(&draining, &drained_id).await, ["shutdown"]);
}

/// TC-SHUTDOWN-3: a caller's interrupt is not relabelled by a later drain.
///
/// A user pressed stop; the process then began stopping. The turn is ending
/// for the reason it was already ending for, and overwriting it would credit a
/// deployment's restart with a decision a person made.
///
/// Input: a turn interrupted by a caller, then drained before it closes.
/// Expected: `"cancelled"` on the journal, not `"shutdown"`.
#[tokio::test]
async fn a_drain_does_not_relabel_an_earlier_interrupt() {
    let gate = Gate::new();
    let (engine, _dir) = engine(Arc::clone(&gate));
    let id = new_session(&engine).await;

    let (_result, ()) = tokio::join!(async { prompt(&engine, &id, "stop me").await }, async {
        gate.entered.notified().await;
        engine
            .agent_interrupt(tetanus_protocol::methods::SessionRef {
                session_id: id.clone(),
            })
            .await
            .expect("interrupt");
        let outstanding = engine.drain(Duration::from_secs(5));
        gate.release.notify_one();
        outstanding.await;
    });

    assert_eq!(
        stop_reasons(&engine, &id).await,
        ["cancelled"],
        "the drain took credit for a stop the user asked for"
    );
}

/// TC-SHUTDOWN-4: a drain with nothing running returns at once.
///
/// The ordinary case for a server stopped between turns, and the one a
/// deployment meets on most restarts. A drain that waited out its budget with
/// nothing to wait for would add its timeout to every clean shutdown.
///
/// Input: an idle engine, and a session that has already finished a turn.
/// Expected: nothing outstanding, and back well inside the budget.
#[tokio::test]
async fn a_drain_with_nothing_running_returns_at_once() {
    let (engine, _dir) = engine(Gate::open());
    let id = new_session(&engine).await;
    prompt(&engine, &id, "finish before the drain").await;

    let started = Instant::now();
    let left_open = engine.drain(Duration::from_secs(30)).await;

    assert_eq!(left_open, 0);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an idle drain waited: {:?}",
        started.elapsed()
    );
}

/// TC-SHUTDOWN-5: a drain that runs out of time says what it left open.
///
/// Best effort, with the crash path still behind it. A tool that will not
/// return cannot be waited for indefinitely, so the wait is bounded and the
/// caller is told - which is what lets a process decide between exiting and
/// waiting longer, and what makes `"interrupted"` after a restart mean the
/// drain did not finish rather than something worse.
///
/// Input: a turn parked in a provider call that is never released until after
/// the drain's budget has passed.
/// Expected: the drain answers one turn still open, and the turn closes
/// normally once released.
#[tokio::test]
async fn a_drain_that_runs_out_of_time_says_so() {
    let gate = Gate::new();
    let (engine, _dir) = engine(Arc::clone(&gate));
    let id = new_session(&engine).await;

    let (_result, left_open) = tokio::join!(
        async { prompt(&engine, &id, "park until after the budget").await },
        async {
            gate.entered.notified().await;
            let outstanding = engine.drain(Duration::from_millis(50)).await;
            // Released only after the budget has already run out, which is the
            // tool-that-will-not-return case in miniature.
            gate.release.notify_one();
            outstanding
        }
    );

    assert_eq!(
        left_open, 1,
        "a bounded drain must report what it could not close"
    );
}

/// A parked provider call, so a case can drain a turn that is genuinely in
/// flight.
struct Gate {
    entered: Notify,
    release: Notify,
    requests: AtomicU32,
    parked: AtomicBool,
    /// An open gate never parks, for the cases that want a turn to finish.
    open: bool,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Notify::new(),
            requests: AtomicU32::new(0),
            parked: AtomicBool::new(false),
            open: false,
        })
    }

    fn open() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Notify::new(),
            requests: AtomicU32::new(0),
            parked: AtomicBool::new(false),
            open: true,
        })
    }
}

struct GatedAdapter {
    inner: mock::MockAdapter,
    gate: Arc<Gate>,
}

#[async_trait::async_trait]
impl LlmAdapter for GatedAdapter {
    fn provider(&self) -> &str {
        self.inner.provider()
    }

    fn models(&self) -> Vec<String> {
        self.inner.models()
    }

    async fn stream(
        &self,
        request: &ModelRequest,
        sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError> {
        self.gate.requests.fetch_add(1, Ordering::Relaxed);
        if !self.gate.open && !self.gate.parked.swap(true, Ordering::Relaxed) {
            self.gate.entered.notify_one();
            self.gate.release.notified().await;
        }
        self.inner.stream(request, sink).await
    }
}

struct One(Arc<dyn LlmAdapter>);

impl Providers for One {
    fn all(&self) -> Vec<Arc<dyn LlmAdapter>> {
        vec![Arc::clone(&self.0)]
    }
}

fn engine(gate: Arc<Gate>) -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let adapter: Arc<dyn LlmAdapter> = Arc::new(GatedAdapter {
        inner: mock::MockAdapter::new(),
        gate,
    });
    let config = EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        providers: Arc::new(One(adapter)),
        ..EngineConfig::default()
    };
    (HarnessEngine::new(config), dir)
}

async fn new_session(engine: &HarnessEngine) -> String {
    engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create")
        .session_id
}

async fn prompt(
    engine: &HarnessEngine,
    id: &str,
    text: &str,
) -> tetanus_protocol::methods::AgentPromptResult {
    engine
        .agent_prompt(AgentPromptParams {
            session_id: id.to_string(),
            content: text.to_string(),
        })
        .await
        .expect("a drained turn answers a summary, not an error")
}

async fn event_types(engine: &HarnessEngine, id: &str) -> Vec<String> {
    engine
        .session_events(SessionEventsParams {
            session_id: id.to_string(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events")
        .events
        .into_iter()
        .map(|event| event.ty)
        .collect()
}

async fn stop_reasons(engine: &HarnessEngine, id: &str) -> Vec<String> {
    engine
        .session_events(SessionEventsParams {
            session_id: id.to_string(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events")
        .events
        .into_iter()
        .filter(|event| event.ty == "turn/end")
        .filter_map(|event| {
            event
                .data
                .get("stop_reason")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect()
}
