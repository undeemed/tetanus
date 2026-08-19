//! Conformance for the agent runtime: `agent.prompt`.
//!
//! Test design: every case runs on the deterministic mock adapter, so none
//! needs a key or a network. Watching a turn that is still in flight needs a
//! gated adapter, which arrives with `agent.status` in the next slice; these
//! cases assert what a closed turn reports.

use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{AgentPromptParams, Engine, SessionCreateParams};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::types::StopReason;

fn engine() -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });
    (engine, dir)
}

async fn session(engine: &HarnessEngine) -> String {
    engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create")
        .session_id
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
