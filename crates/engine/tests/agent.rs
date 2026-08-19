//! Conformance for the agent runtime: `agent.prompt` and `agent.status`.
//!
//! Test design: every case runs on the deterministic mock adapter, or on a
//! gated one written here, so none needs a key or a network. The gated
//! adapter is how a case observes a turn that is still in flight, which is
//! the only way to assert `SessionBusy` and the running state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tetanus_engine::agent::Providers;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    AgentPromptParams, AgentStatusPush, Engine, EventSink, SessionCreateParams, SessionEventPush,
    SessionEventsParams, SessionRef, SessionSubscribeParams,
};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::types::{AgentState, StopReason};
use tetanus_turn::llm::{ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse};

/// One ordered record of everything a carrier would have written, both kinds
/// of push in the order they arrived. Asserting the order is the point: a
/// surface renders "running" before the first event of the turn.
#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<String>>,
}

impl Recorder {
    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("seen").clone()
    }
}

impl EventSink for Recorder {
    fn session_event(&self, push: SessionEventPush) {
        self.seen
            .lock()
            .expect("seen")
            .push(format!("event:{}", push.event.ty));
    }
    fn agent_status(&self, push: AgentStatusPush) {
        let state = match push.state {
            AgentState::Idle => "idle".to_string(),
            AgentState::Running => "running".to_string(),
            AgentState::Other(other) => other,
        };
        self.seen
            .lock()
            .expect("seen")
            .push(format!("status:{state}"));
    }
}

/// An adapter that answers only when a case lets it, so the case can look at
/// a session while its turn is still running.
struct GateAdapter {
    gate: Arc<tokio::sync::Semaphore>,
    entered: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl LlmAdapter for GateAdapter {
    fn provider(&self) -> &str {
        "gate"
    }
    fn models(&self) -> Vec<String> {
        vec!["gate-1".to_string()]
    }
    async fn stream(
        &self,
        _request: &ModelRequest,
        _sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError> {
        self.entered.store(true, Ordering::Release);
        self.gate.acquire().await.expect("gate").forget();
        Ok(ModelResponse {
            content: "held, then answered".into(),
            finish_reason: "stop".into(),
            ..ModelResponse::default()
        })
    }
}

/// The one adapter this build knows, whatever its name.
struct OneProvider(Arc<dyn LlmAdapter>);

impl Providers for OneProvider {
    fn adapter(&self, provider: &str) -> Option<Arc<dyn LlmAdapter>> {
        (provider == self.0.provider()).then(|| Arc::clone(&self.0))
    }
}

fn engine() -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });
    (engine, dir)
}

/// An engine whose turns stop inside the provider call until the returned
/// gate is opened.
fn gated_engine() -> (
    Arc<HarnessEngine>,
    Arc<tokio::sync::Semaphore>,
    Arc<AtomicBool>,
    TempDir,
) {
    let dir = TempDir::new().expect("temp dir");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let entered = Arc::new(AtomicBool::new(false));
    let adapter: Arc<dyn LlmAdapter> = Arc::new(GateAdapter {
        gate: Arc::clone(&gate),
        entered: Arc::clone(&entered),
    });
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        default_provider: "gate".into(),
        default_model: "gate-1".into(),
        providers: Arc::new(OneProvider(adapter)),
        ..EngineConfig::default()
    });
    (Arc::new(engine), gate, entered, dir)
}

async fn session(engine: &HarnessEngine) -> String {
    engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create")
        .session_id
}

/// Wait for a condition a running turn will make true. Bounded, so a broken
/// engine fails the case instead of hanging the suite.
async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if ready() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    panic!("timed out waiting for {what}");
}

/// TC-AGENT-1: a prompt runs the documented turn and answers with its
/// summary. The mock turn calls a tool and then answers, so a natural stop
/// after two steps is the expected result, not merely "it returned".
#[tokio::test]
async fn a_prompt_runs_one_documented_turn() {
    let (engine, _dir) = engine();
    let id = session(&engine).await;

    let summary = engine
        .agent_prompt(AgentPromptParams {
            session_id: id,
            content: "hello there".into(),
        })
        .await
        .expect("prompt")
        .summary;

    assert_eq!(summary.turn, 1);
    assert_eq!(summary.steps, 2, "the mock calls a tool, then answers");
    assert_eq!(summary.stop_reason, StopReason::Natural);
    assert_eq!(summary.stop_veto, None);
    assert!(
        summary.content.contains("hello there"),
        "the answer echoes the prompt: {}",
        summary.content
    );
    assert!(summary.duration_ms.is_some(), "the turn was measured");
    let usage = summary.usage.expect("the mock reports usage");
    assert!(usage.prompt_tokens > 0 && usage.completion_tokens > 0);
}

/// TC-AGENT-2: the turn's durable facts reach a subscriber as they happen,
/// and they are the journal's own events: the pushed sequence equals what
/// `session.events` reports afterwards.
#[tokio::test]
async fn a_turn_pushes_its_journal_to_a_subscriber() {
    let (engine, _dir) = engine();
    let id = session(&engine).await;
    let sink = Arc::new(Recorder::default());

    engine
        .session_subscribe(
            SessionSubscribeParams {
                session_id: id.clone(),
                from_seq: None,
            },
            Arc::clone(&sink) as Arc<dyn EventSink>,
        )
        .await
        .expect("subscribe");

    engine
        .agent_prompt(AgentPromptParams {
            session_id: id.clone(),
            content: "hello there".into(),
        })
        .await
        .expect("prompt");

    let journal = engine
        .session_events(SessionEventsParams {
            session_id: id,
            from_seq: 1,
            limit: None,
        })
        .await
        .expect("events");
    let pushed: Vec<String> = sink
        .seen()
        .into_iter()
        .filter(|line| line.starts_with("event:"))
        .collect();
    let written: Vec<String> = journal
        .events
        .iter()
        .map(|event| format!("event:{}", event.ty))
        .collect();
    assert_eq!(pushed, written, "a subscriber sees the journal, in order");
    assert!(written.iter().any(|ty| ty == "event:turn/end"));
}

/// TC-AGENT-3: `agent/status` is pushed on both transitions, and it brackets
/// the turn: running before the first event, idle after the last.
#[tokio::test]
async fn status_is_pushed_on_each_transition() {
    let (engine, _dir) = engine();
    let id = session(&engine).await;
    let sink = Arc::new(Recorder::default());

    engine
        .session_subscribe(
            SessionSubscribeParams {
                session_id: id.clone(),
                from_seq: None,
            },
            Arc::clone(&sink) as Arc<dyn EventSink>,
        )
        .await
        .expect("subscribe");
    engine
        .agent_prompt(AgentPromptParams {
            session_id: id,
            content: "hello there".into(),
        })
        .await
        .expect("prompt");

    let seen = sink.seen();
    assert_eq!(seen.first().map(String::as_str), Some("status:running"));
    assert_eq!(seen.last().map(String::as_str), Some("status:idle"));
    assert_eq!(
        seen.iter()
            .filter(|line| line.starts_with("status:"))
            .count(),
        2,
        "one turn is two transitions, not one per step"
    );
    assert_eq!(seen[1], "event:turn/start");
}

/// TC-AGENT-4: a second prompt while a turn is in flight is `SessionBusy`,
/// naming the session and the turn that holds it. The first turn is not
/// disturbed: it still answers.
#[tokio::test]
async fn a_second_prompt_while_running_is_busy() {
    let (engine, gate, entered, _dir) = gated_engine();
    let id = session(&engine).await;

    let running = tokio::spawn({
        let engine = Arc::clone(&engine);
        let id = id.clone();
        async move {
            engine
                .agent_prompt(AgentPromptParams {
                    session_id: id,
                    content: "first".into(),
                })
                .await
        }
    });
    until("the turn to reach the provider", || {
        entered.load(Ordering::Acquire)
    })
    .await;

    let refused = engine
        .agent_prompt(AgentPromptParams {
            session_id: id.clone(),
            content: "second".into(),
        })
        .await
        .expect_err("a turn is already running");
    assert_eq!(refused.kind(), Some(ErrorCode::SessionBusy));
    let data = refused.data.expect("data");
    assert_eq!(data["session_id"], serde_json::json!(id));
    assert_eq!(data["turn"], serde_json::json!(1), "the turn in flight");

    gate.add_permits(1);
    let summary = running.await.expect("join").expect("prompt").summary;
    assert_eq!(summary.turn, 1);
    assert_eq!(summary.usage, None, "this adapter measures nothing");
}

/// TC-AGENT-5: `agent.status` reads the live state, which is idle before and
/// after a turn and running with its progress during one. This is how a
/// surface that missed a push resynchronises.
#[tokio::test]
async fn status_reads_the_live_state() {
    let (engine, gate, entered, _dir) = gated_engine();
    let id = session(&engine).await;
    let status = |id: String| {
        let engine = Arc::clone(&engine);
        async move {
            engine
                .agent_status(SessionRef { session_id: id })
                .await
                .expect("status")
                .status
        }
    };

    let before = status(id.clone()).await;
    assert_eq!(before.state, AgentState::Idle);
    assert_eq!(before.turn, None);

    let running = tokio::spawn({
        let engine = Arc::clone(&engine);
        let id = id.clone();
        async move {
            engine
                .agent_prompt(AgentPromptParams {
                    session_id: id,
                    content: "first".into(),
                })
                .await
        }
    });
    until("the turn to reach the provider", || {
        entered.load(Ordering::Acquire)
    })
    .await;

    let during = status(id.clone()).await;
    assert_eq!(during.state, AgentState::Running);
    assert_eq!(during.turn, Some(1));
    assert_eq!(during.step, Some(1), "the step the journal is on");

    gate.add_permits(1);
    running.await.expect("join").expect("prompt");
    let after = status(id).await;
    assert_eq!(after.state, AgentState::Idle);
    assert_eq!(after.step, None);
}

/// TC-AGENT-6: a session resumed after a restart continues its turn
/// numbering. Two turns in one journal never share an id, so a surface can
/// group events by turn.
#[tokio::test]
async fn a_resumed_session_continues_its_turn_numbering() {
    let dir = TempDir::new().expect("temp dir");
    let config = EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    };

    let first = HarnessEngine::new(config.clone());
    let id = session(&first).await;
    let one = first
        .agent_prompt(AgentPromptParams {
            session_id: id.clone(),
            content: "first".into(),
        })
        .await
        .expect("prompt")
        .summary;
    assert_eq!(one.turn, 1);
    drop(first);

    let restarted = HarnessEngine::new(config);
    let two = restarted
        .agent_prompt(AgentPromptParams {
            session_id: id,
            content: "second".into(),
        })
        .await
        .expect("prompt")
        .summary;
    assert_eq!(two.turn, 2, "numbering continues after the journal's turns");
}

/// TC-AGENT-7: the two ways a prompt is refused before a turn starts. An
/// unknown session is `SessionNotFound`; a session whose header names a
/// provider this build has no adapter for names the faulty field.
#[tokio::test]
async fn a_prompt_that_cannot_start_says_which_input_is_wrong() {
    let (engine, dir) = engine();

    let missing = engine
        .agent_prompt(AgentPromptParams {
            session_id: "nowhere".into(),
            content: "hi".into(),
        })
        .await
        .expect_err("no such session");
    assert_eq!(missing.kind(), Some(ErrorCode::SessionNotFound));

    let foreign = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        default_provider: "not-in-this-build".into(),
        ..EngineConfig::default()
    });
    let id = session(&foreign).await;
    let refused = foreign
        .agent_prompt(AgentPromptParams {
            session_id: id,
            content: "hi".into(),
        })
        .await
        .expect_err("no adapter for that provider");
    assert_eq!(refused.kind(), Some(ErrorCode::InvalidParams));
    assert_eq!(
        refused.data.expect("data")["field"],
        serde_json::json!("provider")
    );
}
