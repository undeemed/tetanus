//! Test Design Specification: the resolved retry policy on a live route.
//!
//! Feature under test: the installation in `tetanus_engine::agent` - the
//! engine putting the policy `tetanus_engine::retry` resolved onto the route
//! each session names.
//!
//! Approach: both cases run a whole `agent.prompt` against an adapter that
//! fails on demand, configured only by a document. Nothing here passes a
//! policy in, so the only way the observed behaviour happens is the engine
//! installing what it read.
//!
//! Features NOT tested here: the resolution itself
//! (`crates/engine/tests/retry.rs`), what the executor does with a policy
//! (`crates/turn/tests/upstream_retry_executor.rs`) and what the policy decides
//! (`upstream_retry_policy.rs`).
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
use tetanus_engine::{boot, EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    AgentPromptParams, Engine, SessionCreateParams, SessionEventsParams,
};
use tetanus_turn::llm::retry::RETRY_EVENT;
use tetanus_turn::llm::{ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse};

/// TC-RETRY-6: the resolved policy is installed on the route the session names.
///
/// Input: a document allowing one retry of `SERVER` with a one-millisecond
/// wait, and a provider whose first request fails with 503.
/// Expected: the prompt answers normally, the provider was called twice, and
/// the journal carries the `llm/retry` the executor writes before its wait.
/// Nothing here passes a policy in, so the only way the retry happens is the
/// engine installing what it read.
#[tokio::test]
async fn the_document_policy_runs_against_the_session_route() {
    let (engine, attempts, _dir) = flaky_engine("max_retries: 1");
    let session = open(&engine).await;

    let answered = engine
        .agent_prompt(AgentPromptParams {
            session_id: session.clone(),
            content: "please answer".to_string(),
        })
        .await
        .expect("the turn ran");

    assert_eq!(answered.summary.content, "answered on attempt 2");
    assert_eq!(attempts.load(Ordering::Acquire), 2);
    assert!(
        journal(&engine, &session)
            .await
            .contains(&RETRY_EVENT.to_string()),
        "the scheduled retry is not on the journal"
    );
}

/// TC-RETRY-7: a document that allows no retry gets none.
///
/// Input: the same failing provider, under `max_retries: 0`.
/// Expected: the prompt fails, and the provider was called once. This is the
/// case that proves the number came from the document: the compiled default
/// allows two retries, and under it this turn would have succeeded.
#[tokio::test]
async fn a_document_that_allows_no_retry_fails_the_turn() {
    let (engine, attempts, _dir) = flaky_engine("max_retries: 0");
    let session = open(&engine).await;

    let failed = engine
        .agent_prompt(AgentPromptParams {
            session_id: session,
            content: "please answer".to_string(),
        })
        .await
        .expect_err("the only attempt failed");

    assert!(failed.message.contains("503"), "{failed:?}");
    assert_eq!(attempts.load(Ordering::Acquire), 1);
}

const FLAKY: &str = "flaky";

/// A provider whose first request fails with a retryable status and whose
/// second answers, so a case can tell a retry from a first attempt.
struct Flaky(Arc<AtomicU32>);

#[async_trait::async_trait]
impl LlmAdapter for Flaky {
    fn provider(&self) -> &str {
        FLAKY
    }
    fn models(&self) -> Vec<String> {
        vec!["flaky-1".to_string()]
    }
    async fn stream(
        &self,
        _request: &ModelRequest,
        _sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError> {
        let attempt = self.0.fetch_add(1, Ordering::AcqRel) + 1;
        if attempt == 1 {
            return Err(LlmError::Provider {
                status: 503,
                message: "the route is busy".to_string(),
                retry_after_ms: None,
            });
        }
        Ok(ModelResponse {
            content: format!("answered on attempt {attempt}"),
            finish_reason: "stop".to_string(),
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

/// A document holding `settings` under `llm.retry`, and the config it resolves
/// to over the engine's own defaults.
fn document(text: &str) -> (TempDir, tetanus_config::Config) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    std::fs::write(&path, text).expect("write");
    let settings = boot::document(&path).expect("read");
    (dir, settings)
}

/// An engine on the failing provider, configured by a document rather than by
/// this function, and the attempt counter that provider increments.
fn flaky_engine(setting: &str) -> (HarnessEngine, Arc<AtomicU32>, TempDir) {
    let attempts = Arc::new(AtomicU32::new(0));
    let (dir, settings) = document(&format!(
        "provider:
  default: {FLAKY}
model:
  default: flaky-1
llm:
  retry:
    mode: normal
    retryable_codes: [SERVER]
    {setting}
    backoff:
      initial_delay_ms: 1
      max_delay_ms: 2"
    ));
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        providers: Arc::new(One(Arc::new(Flaky(Arc::clone(&attempts))))),
        ..EngineConfig::from_settings(settings).expect("settings")
    });
    (engine, attempts, dir)
}

async fn open(engine: &HarnessEngine) -> String {
    engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("session")
        .session_id
}

async fn journal(engine: &HarnessEngine, session_id: &str) -> Vec<String> {
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
        .map(|event| event.ty)
        .collect()
}
