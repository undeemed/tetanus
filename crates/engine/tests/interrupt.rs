//! Test Design Specification: upstream cancellation, ported.
//!
//! Feature under test: `agent.interrupt`, and what it leaves on the journal.
//! Upstream pins cancellation in `packages/core/agent-loop/tests/cancel.spec.ts`;
//! each case names the upstream case it comes from.
//!
//! Approach: the real `Engine` calls, against a temporary journal root and an
//! adapter that parks its first request, so a case interrupts a turn that is
//! genuinely in flight rather than a simulation of one. No case needs a key or
//! a network.
//!
//! Upstream's spec is 27 cases. Most of them are about surfaces tetanus has
//! not built: a queued inbox, steering, latched wakes, `whenIdle` waiters, and
//! fiber disposal as a separate cause from cancellation. Those are not
//! restated here as passing tests; they stay rows in `docs/parity.md`.
//!
//! Two more are already pinned elsewhere and are not restated twice.
//! `crates/engine/tests/agent.rs` TC-AGENT-9 is upstream's "cancel() on an
//! idle agent is a no-op; the next prompt runs", and TC-AGENT-8 is its
//! "cancel() mid-step aborts the active turn". TC-PORT-CANCEL-1 below takes up
//! only what TC-AGENT-8 does not claim.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use tempfile::TempDir;
use tokio::sync::Notify;

use tetanus_engine::agent::Providers;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    AgentPromptParams, Engine, SessionCreateParams, SessionEventsParams, SessionRef,
};
use tetanus_protocol::types::{AgentState, StopReason};
use tetanus_turn::llm::{mock, ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse};

/// TC-PORT-CANCEL-1: an interrupted turn leaves a balanced journal, and asks
/// the provider nothing more.
///
/// Upstream: "cancel() mid-step aborts the active turn and drops every queued
/// tail item". TC-AGENT-8 already pins the outcome a caller sees - the ack,
/// `stop_reason: cancelled`, and one step. What it does not claim, and what
/// upstream's case is really about, is the state the journal is left in and
/// the work that is dropped rather than merely ignored.
///
/// Input: the adapter parks inside step 1's request; the interrupt lands while
/// it is parked; the request is then released.
/// Expected: exactly one provider request over the whole turn, and a balanced
/// journal - the open step is closed, and `turn/end` is the last event.
#[tokio::test]
async fn an_interrupt_ends_the_turn_at_the_step_boundary() {
    let gate = Gate::new();
    let (engine, _dir) = engine(Some(Arc::clone(&gate)));
    let id = new_session(&engine).await;

    let (result, ()) = tokio::join!(
        async { prompt(&engine, &id, "interrupt me").await },
        async {
            gate.entered.notified().await;
            let ack = engine
                .agent_interrupt(SessionRef {
                    session_id: id.clone(),
                })
                .await
                .expect("interrupt");
            assert!(ack.ok, "a turn was in flight, so the ask landed");
            gate.release.notify_one();
        }
    );

    assert_eq!(result.summary.stop_reason, StopReason::Cancelled);
    assert_eq!(result.summary.steps, 1, "the second step never ran");
    assert_eq!(
        gate.requests.load(Ordering::Relaxed),
        1,
        "no provider request after the interrupt"
    );

    let types = event_types(&engine, &id).await;
    assert_eq!(
        types.iter().filter(|t| *t == "step/start").count(),
        1,
        "one step opened: {types:?}"
    );
    assert_eq!(
        types.iter().filter(|t| *t == "step/end").count(),
        1,
        "and it was closed: {types:?}"
    );
    assert_eq!(
        types.last().map(String::as_str),
        Some("turn/end"),
        "an interrupted turn is still a balanced turn: {types:?}"
    );
}

/// TC-PORT-CANCEL-2: the interrupt applies to the turn it caught, and not to
/// the next one.
///
/// Upstream: "a prompt sent AFTER a cancelled turn settles runs normally
/// (marker reset)".
///
/// Expected: the second prompt stops naturally with an answer, and its turn
/// number continues from the cancelled one. Its step count is not asserted:
/// the offline mock reads the derived history, and the cancelled turn left a
/// tool result on the journal, so the next turn needs no tool step. That is
/// the fixture's arithmetic, not the marker's.
#[tokio::test]
async fn a_prompt_after_a_cancelled_turn_runs_normally() {
    let gate = Gate::new();
    let (engine, _dir) = engine(Some(Arc::clone(&gate)));
    let id = new_session(&engine).await;

    let (cancelled, ()) = tokio::join!(
        async { prompt(&engine, &id, "cancel this one").await },
        async {
            gate.entered.notified().await;
            engine
                .agent_interrupt(SessionRef {
                    session_id: id.clone(),
                })
                .await
                .expect("interrupt");
            gate.release.notify_one();
        }
    );
    assert_eq!(cancelled.summary.stop_reason, StopReason::Cancelled);

    let next = prompt(&engine, &id, "and run this one").await;
    assert_eq!(next.summary.stop_reason, StopReason::Natural);
    assert!(
        next.summary.steps >= 1,
        "the next turn is not stopped short"
    );
    assert!(!next.summary.content.is_empty(), "and it answers");
    assert_eq!(next.summary.turn, cancelled.summary.turn + 1);
}

/// TC-PORT-CANCEL-3: two callers interrupting one turn is not a fault.
///
/// Upstream: "keeps the first typed cause for an active turn" - a second
/// cancellation does not replace the first, and does not error.
///
/// Expected: the first ask reports `ok: true` and the second `ok: false`, both
/// succeed as calls, and the turn ends cancelled once.
#[tokio::test]
async fn a_second_interrupt_on_one_turn_is_not_a_fault() {
    let gate = Gate::new();
    let (engine, _dir) = engine(Some(Arc::clone(&gate)));
    let id = new_session(&engine).await;

    let (result, acks) = tokio::join!(
        async { prompt(&engine, &id, "interrupt me twice").await },
        async {
            gate.entered.notified().await;
            let reference = SessionRef {
                session_id: id.clone(),
            };
            let first = engine
                .agent_interrupt(reference.clone())
                .await
                .expect("first interrupt");
            let second = engine
                .agent_interrupt(reference)
                .await
                .expect("second interrupt");
            gate.release.notify_one();
            (first.ok, second.ok)
        }
    );

    assert_eq!(acks, (true, false), "the first ask is the one that lands");
    assert_eq!(result.summary.stop_reason, StopReason::Cancelled);
}

/// TC-PORT-CANCEL-4: an interrupt does not relabel a turn that was finishing
/// anyway.
///
/// Upstream: "cancel during the stopping window ends the turn aborted and runs
/// no further step". tetanus deliberately reads the flag only where another
/// step would be claimed, so a turn whose last step already answered ends
/// `natural`. The observable half upstream also asserts - no further step -
/// holds either way, and the difference is a row in `docs/parity.md`.
///
/// Input: the adapter parks inside the *second* request, which is the one that
/// answers, so the interrupt lands after the last step it could stop.
/// Expected: two steps, `stop_reason: natural`, and the session is idle again.
#[tokio::test]
async fn an_interrupt_after_the_last_step_does_not_relabel_the_turn() {
    let gate = Gate::new_at(2);
    let (engine, _dir) = engine(Some(Arc::clone(&gate)));
    let id = new_session(&engine).await;

    let (result, ()) = tokio::join!(async { prompt(&engine, &id, "too late").await }, async {
        gate.entered.notified().await;
        engine
            .agent_interrupt(SessionRef {
                session_id: id.clone(),
            })
            .await
            .expect("interrupt");
        gate.release.notify_one();
    });

    assert_eq!(result.summary.stop_reason, StopReason::Natural);
    assert_eq!(result.summary.steps, 2);

    let status = engine
        .agent_status(SessionRef { session_id: id })
        .await
        .expect("status");
    assert_eq!(status.status.state, AgentState::Idle);
}

/// TC-PORT-CANCEL-5: interrupting a session that does not exist is the
/// documented error, not a silent acknowledgement.
///
/// Upstream: cancel is only reachable through an agent handle, so this has no
/// upstream case. It is contract section 4.5's rule, asserted where the other
/// cancellation cases live, because an id is the only thing this call takes.
///
/// Expected: `SessionNotFound`.
#[tokio::test]
async fn interrupting_an_unknown_session_is_session_not_found() {
    let (engine, _dir) = engine(None);

    let error = engine
        .agent_interrupt(SessionRef {
            session_id: "never-created".into(),
        })
        .await
        .expect_err("no such session");
    assert_eq!(
        error.kind(),
        Some(tetanus_protocol::rpc::ErrorCode::SessionNotFound)
    );
}

/// A rendezvous with the provider: the adapter parks inside one request until
/// a case releases it, so the interrupt lands mid-turn every run.
struct Gate {
    /// Raised by the adapter once it is parked.
    entered: Notify,
    /// Raised by the case to let the parked request finish.
    release: Notify,
    /// Requests the adapter has been asked to make, parked or not.
    requests: AtomicU32,
    /// Which request to park on: 1 is the step that calls the tool, 2 is the
    /// step that answers.
    park_at: u32,
    parked: AtomicBool,
}

impl Gate {
    fn new() -> Arc<Self> {
        Self::new_at(1)
    }

    fn new_at(park_at: u32) -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Notify::new(),
            requests: AtomicU32::new(0),
            park_at,
            parked: AtomicBool::new(false),
        })
    }
}

/// The offline mock, with one request held open.
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
        let nth = self.gate.requests.fetch_add(1, Ordering::AcqRel) + 1;
        if nth == self.gate.park_at && !self.gate.parked.swap(true, Ordering::AcqRel) {
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

/// An engine over a temporary journal root. With a gate, its provider parks;
/// without one, it is the plain offline mock.
fn engine(gate: Option<Arc<Gate>>) -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let mut config = EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    };
    if let Some(gate) = gate {
        let adapter: Arc<dyn LlmAdapter> = Arc::new(GatedAdapter {
            inner: mock::MockAdapter::new(),
            gate,
        });
        config.providers = Arc::new(One(adapter));
    }
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
    session_id: &str,
    content: &str,
) -> tetanus_protocol::methods::AgentPromptResult {
    engine
        .agent_prompt(AgentPromptParams {
            session_id: session_id.to_string(),
            content: content.to_string(),
        })
        .await
        .expect("prompt")
}

async fn event_types(engine: &HarnessEngine, session_id: &str) -> Vec<String> {
    engine
        .session_events(SessionEventsParams {
            session_id: session_id.to_string(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events")
        .events
        .into_iter()
        .map(|e| e.ty)
        .collect()
}
