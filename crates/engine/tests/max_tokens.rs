//! Test Design Specification: a turn cut off at the output cap, as a caller
//! sees it.
//!
//! Feature under test: how the reason `crates/turn` ends such a turn with
//! crosses the published boundary - the `stop_reason` `agent.prompt` answers,
//! the word `session.events` carries for the same turn, and the state the
//! session is left in. `docs/interface-contract.md` section 4.4.2 states the
//! rule; section 7.5 is why the reason travels as a value of the growable
//! `StopReason` rather than as a variant of it.
//!
//! Approach: the real `Engine` calls, against a temporary journal root and an
//! adapter whose first answer is cut off at the cap. Nothing here reaches into
//! the turn engine: a case asks the same calls a surface asks.
//!
//! Features NOT tested here: what the turn engine does inside the cut-off step
//! (`crates/turn/tests/upstream_max_tokens.rs`), and how a surface prints the
//! reason, which is the presentation lane's.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use tempfile::TempDir;

use tetanus_engine::agent::Providers;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    AgentPromptParams, AgentPromptResult, Engine, SessionCreateParams, SessionEventsParams,
    SessionRef,
};
use tetanus_protocol::types::{AgentState, SessionEvent, StopReason};
use tetanus_turn::llm::{mock, ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse};
use tetanus_turn::tools::ToolCall;

/// TC-CAP-1: a turn the cap cut off says so to its caller and on its journal.
///
/// Input: a session on a route whose answer stops at the output cap, prompted
/// once.
/// Expected: the summary reads `Other("max-tokens")` over one step, and the
/// durable `turn/end` carries the same word. The two are asserted together
/// because they are the two ways a client can learn the answer is unfinished,
/// and a client that resynchronises from the journal must not read a different
/// reason from the one the call returned.
#[tokio::test]
async fn a_turn_the_cap_cut_off_reports_max_tokens() {
    let (engine, _dir) = engine(vec![]);
    let id = new_session(&engine).await;

    let answered = prompt(&engine, &id, "go").await;

    assert_eq!(
        answered.summary.stop_reason,
        StopReason::Other("max-tokens".into())
    );
    assert_eq!(answered.summary.steps, 1);
    assert_eq!(answered.summary.content, "half an ans");
    let journal = events(&engine, &id).await;
    let end = last(&journal, "turn/end");
    assert_eq!(end.data["stop_reason"], serde_json::json!("max-tokens"));
}

/// TC-CAP-2: the cut-off turn dispatches nothing and leaves the session usable.
///
/// Input: the same route, whose cut-off answer carries a well-formed call on
/// the engine's own `echo` tool, and a second prompt after it.
/// Expected: no `tool/call` reaches the journal, the session is idle again,
/// and the next turn ends `Natural`. A turn that stops for a reason the
/// contract added after 1.0 must still release the session: a caller whose
/// answer was cut off has every reason to prompt again.
#[tokio::test]
async fn a_cut_off_turn_runs_no_tool_and_still_releases_the_session() {
    let (engine, _dir) = engine(vec![ToolCall {
        id: "c1".into(),
        name: "echo".into(),
        arguments: serde_json::json!({ "text": "x" }),
    }]);
    let id = new_session(&engine).await;

    let cut_off = prompt(&engine, &id, "go").await;
    // Read before the second prompt: the mock answering that one calls a tool
    // of its own, and the question here is what the cut-off turn dispatched.
    let after_the_cut = events(&engine, &id).await;
    let status = engine
        .agent_status(SessionRef {
            session_id: id.clone(),
        })
        .await
        .expect("status");
    let next = prompt(&engine, &id, "carry on").await;

    assert_eq!(
        cut_off.summary.stop_reason,
        StopReason::Other("max-tokens".into())
    );
    assert!(
        !after_the_cut.iter().any(|event| event.ty == "tool/call"),
        "a call the cap cut off was dispatched"
    );
    assert_eq!(status.status.state, AgentState::Idle);
    assert_eq!(next.summary.stop_reason, StopReason::Natural);
}

/// A route whose first answer stops at the provider's output cap, carrying the
/// text it had written and whatever calls it had started. Every later request
/// is the offline mock's, so a case can prompt again after the cut.
struct Capped {
    inner: mock::MockAdapter,
    calls: Vec<ToolCall>,
    requests: AtomicU32,
}

#[async_trait::async_trait]
impl LlmAdapter for Capped {
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
        if self.requests.fetch_add(1, Ordering::AcqRel) > 0 {
            return self.inner.stream(request, sink).await;
        }
        Ok(ModelResponse {
            content: "half an ans".into(),
            tool_calls: self.calls.clone(),
            // The word the OpenAI-compatible wire DeepSeek serves uses for the
            // output cap.
            finish_reason: "length".into(),
            ..ModelResponse::default()
        })
    }
}

struct One(Arc<dyn LlmAdapter>);

impl Providers for One {
    fn all(&self) -> Vec<Arc<dyn LlmAdapter>> {
        vec![Arc::clone(&self.0)]
    }
}

fn engine(calls: Vec<ToolCall>) -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let adapter: Arc<dyn LlmAdapter> = Arc::new(Capped {
        inner: mock::MockAdapter::new(),
        calls,
        requests: AtomicU32::new(0),
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
        .expect("session")
        .session_id
}

async fn prompt(engine: &HarnessEngine, session_id: &str, content: &str) -> AgentPromptResult {
    engine
        .agent_prompt(AgentPromptParams {
            session_id: session_id.to_string(),
            content: content.to_string(),
        })
        .await
        .expect("the turn ran")
}

async fn events(engine: &HarnessEngine, session_id: &str) -> Vec<SessionEvent> {
    engine
        .session_events(SessionEventsParams {
            session_id: session_id.to_string(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events")
        .events
}

fn last<'a>(events: &'a [SessionEvent], ty: &str) -> &'a SessionEvent {
    events
        .iter()
        .rev()
        .find(|event| event.ty == ty)
        .unwrap_or_else(|| panic!("no `{ty}` on the journal"))
}
