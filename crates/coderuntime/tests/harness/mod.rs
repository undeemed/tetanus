//! What the turn cases run against: a scripted model that asks for one named
//! tool, and one booted turn engine over a temporary journal.
//!
//! The same fixture `crates/mcp/tests/harness/mod.rs` uses, for the same
//! reason: the claim is about the tool pipeline, so the pipeline has to be the
//! real one. It is copied rather than shared because a test fixture that two
//! crates depend on is a third crate, and neither of them wants one.

#![allow(dead_code)]

use std::sync::Arc;

use serde_json::Value;

/// A model that asks for one named tool on its first step and answers with
/// whatever came back on its second.
///
/// `tetanus_turn::llm::mock` always asks for `echo`, so a turn about any other
/// tool needs a model that asks for that tool by name. Everything else about
/// the shape - one step that calls, one that answers - is the mock's, so the
/// documented event sequence is the documented event sequence.
pub struct ModelAsking {
    pub tool: String,
    pub arguments: Value,
}

#[async_trait::async_trait]
impl tetanus_turn::llm::LlmAdapter for ModelAsking {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn models(&self) -> Vec<String> {
        vec!["scripted-1".to_string()]
    }

    async fn stream(
        &self,
        request: &tetanus_turn::llm::ModelRequest,
        sink: &mut dyn tetanus_turn::llm::ChunkSink,
    ) -> Result<tetanus_turn::llm::ModelResponse, tetanus_turn::llm::LlmError> {
        use tetanus_turn::llm::{ModelResponse, Role, StreamChunk};

        let answered = request
            .messages
            .last()
            .filter(|message| message.role == Role::Tool);
        if let Some(answer) = answered {
            let content = format!("the tool said: {}", answer.content);
            sink.chunk(StreamChunk::Text {
                delta: content.clone(),
            })
            .await?;
            return Ok(ModelResponse {
                content,
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: "stop".into(),
                usage: None,
            });
        }

        let content = format!("I will use {}.", self.tool);
        sink.chunk(StreamChunk::Text {
            delta: content.clone(),
        })
        .await?;
        let call = tetanus_turn::tools::ToolCall {
            id: "call_1".into(),
            name: self.tool.clone(),
            arguments: self.arguments.clone(),
        };
        sink.chunk(StreamChunk::ToolCall { call: call.clone() })
            .await?;
        Ok(ModelResponse {
            content,
            reasoning: String::new(),
            tool_calls: vec![call],
            finish_reason: "tool_calls".into(),
            usage: None,
        })
    }
}

/// One booted turn engine over a caller-supplied registry and model, writing
/// to a temporary journal.
pub struct TurnFixture {
    pub engine: tetanus_turn::TurnEngine,
    pub log: std::sync::Arc<dyn tetanus_session::SessionLog>,
    _dir: tempfile::TempDir,
}

impl TurnFixture {
    pub fn new(
        name: &str,
        tools: tetanus_turn::tools::ToolRegistry,
        model: Arc<dyn tetanus_turn::llm::LlmAdapter>,
    ) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let bus = tetanus_core::EventBus::new();
        let log: std::sync::Arc<dyn tetanus_session::SessionLog> =
            tetanus_session::JsonlSessionLog::create(
                name,
                dir.path().join("turn.jsonl"),
                bus.clone(),
            )
            .expect("journal");
        let ctx =
            tetanus_turn::boot::boot(bus, model, Arc::new(tools), Arc::clone(&log)).expect("boot");
        let engine = tetanus_turn::TurnEngine::from_context(
            &ctx,
            tetanus_turn::TurnConfig {
                model: "scripted-1".to_string(),
                ..tetanus_turn::TurnConfig::default()
            },
        )
        .expect("engine");
        Self {
            engine,
            log,
            _dir: dir,
        }
    }

    /// The `tool/result` records this journal holds, as (name, ok, content).
    pub fn tool_results(&self) -> Vec<(String, bool, String)> {
        self.log
            .events()
            .iter()
            .filter(|event| event.ty == "tool/result")
            .map(|event| {
                (
                    event.data["name"].as_str().unwrap_or_default().to_string(),
                    event.data["ok"].as_bool().unwrap_or_default(),
                    event.data["content"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect()
    }
}
