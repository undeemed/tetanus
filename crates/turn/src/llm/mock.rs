//! Deterministic offline adapter. It runs the same documented turn as a real
//! provider - one step that calls a tool, one step that answers - so a run with
//! no API key is still a full turn and is reproducible byte for byte.

use crate::llm::{
    ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse, Role, StreamChunk, Usage,
};
use crate::tools::ToolCall;

pub const PROVIDER: &str = "mock";
pub const MODEL: &str = "mock-echo-1";

#[derive(Debug, Default)]
pub struct MockAdapter;

impl MockAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl LlmAdapter for MockAdapter {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn models(&self) -> Vec<String> {
        vec![MODEL.to_string()]
    }

    async fn stream(
        &self,
        request: &ModelRequest,
        sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError> {
        let answered = request.messages.iter().any(|m| m.role == Role::Tool);
        let can_echo = request.tools.iter().any(|t| t.name == "echo");

        if !answered && can_echo {
            let asked = last_content(request, Role::User);
            let content = "Let me echo that back.";
            for delta in ["Let me ", "echo that ", "back."] {
                sink.chunk(StreamChunk::Text {
                    delta: delta.to_string(),
                })
                .await?;
            }
            let call = ToolCall {
                id: "call_1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": asked }),
            };
            sink.chunk(StreamChunk::ToolCall { call: call.clone() })
                .await?;
            return Ok(ModelResponse {
                content: content.into(),
                reasoning: String::new(),
                tool_calls: vec![call],
                finish_reason: "tool_calls".into(),
                usage: Some(usage(request, content)),
            });
        }

        let echoed = last_content(request, Role::Tool);
        let content = format!("You said: {echoed}");
        for delta in ["You said: ", echoed.as_str()] {
            sink.chunk(StreamChunk::Text {
                delta: delta.to_string(),
            })
            .await?;
        }
        Ok(ModelResponse {
            content: content.clone(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: "stop".into(),
            usage: Some(usage(request, &content)),
        })
    }
}

fn last_content(request: &ModelRequest, role: Role) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == role)
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

/// A stand-in token count: deterministic, so two identical runs report the
/// same usage.
fn usage(request: &ModelRequest, completion: &str) -> Usage {
    let prompt: usize = request.messages.iter().map(|m| m.content.len()).sum();
    Usage {
        prompt_tokens: (prompt / 4) as u64,
        completion_tokens: (completion.len() / 4) as u64,
    }
}
