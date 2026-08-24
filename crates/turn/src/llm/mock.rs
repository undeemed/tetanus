//! Deterministic offline adapter. It runs the same documented turn as a real
//! provider - one step that calls a tool, one step that answers - so a run with
//! no API key is still a full turn and is reproducible byte for byte.
//!
//! Every turn runs that shape, not only the first. A request carries the whole
//! conversation, so both of the adapter's questions are asked of this step's
//! own messages: an earlier turn's tool result is history, not an answer to
//! the call this step has not made yet.
//!
//! Which tool it calls is the prompt's to choose. A prompt opening with `!`
//! means "run the rest as a command", and the adapter asks for [`SHELL`] with
//! it; anything else is echoed back. That one convention is what lets a build
//! with no API key demonstrate a real command running through a real turn -
//! the model is the only part of the loop being stood in for, and a mock that
//! could only ever call `echo` would leave the whole shell path unreachable
//! from the binary.

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
        // The result of this step's own call, if the step already has one.
        // Tool results are appended after the assistant message that asked for
        // them, so the last message is a tool result exactly on the step that
        // answers. Reading the decision and the text from one message keeps
        // them from disagreeing.
        let answer = request
            .messages
            .last()
            .filter(|message| message.role == Role::Tool);
        let asked = last_content(request, Role::User);
        let wanted = wanted_call(&asked, request);

        if answer.is_none() {
            let Some((tool, arguments, said)) = wanted else {
                // Neither tool is registered, so there is nothing to call and
                // the step answers directly.
                return answer_now(&asked, sink).await;
            };
            let content = said.concat();
            // Three text chunks and then the call, whichever tool it is: the
            // conformance suite asserts the whole event sequence, and a mock
            // that streamed a different number of pieces for one prompt would
            // make the documented flow depend on what was typed.
            for delta in said {
                sink.chunk(StreamChunk::Text {
                    delta: delta.to_string(),
                })
                .await?;
            }
            let call = ToolCall {
                id: "call_1".into(),
                name: tool.into(),
                arguments,
            };
            sink.chunk(StreamChunk::ToolCall { call: call.clone() })
                .await?;
            return Ok(ModelResponse {
                content: content.clone(),
                reasoning: String::new(),
                tool_calls: vec![call],
                finish_reason: "tool_calls".into(),
                usage: Some(usage(request, &content)),
            });
        }

        let echoed = answer
            .map(|message| message.content.clone())
            .unwrap_or_default();
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

/// The command a `!` prompt asks for: everything after the mark, trimmed.
fn command_of(prompt: &str) -> Option<&str> {
    let command = prompt.strip_prefix('!')?.trim();
    (!command.is_empty()).then_some(command)
}

/// Which tool this step asks for, with what, and the three pieces it says
/// while doing it.
fn wanted_call(
    asked: &str,
    request: &ModelRequest,
) -> Option<(&'static str, serde_json::Value, [&'static str; 3])> {
    let has = |name: &str| request.tools.iter().any(|tool| tool.name == name);
    if let Some(command) = command_of(asked) {
        if has(SHELL) {
            return Some((
                SHELL,
                serde_json::json!({ "command": command }),
                ["Let me ", "run that ", "command."],
            ));
        }
    }
    has("echo").then(|| {
        (
            "echo",
            serde_json::json!({ "text": asked }),
            ["Let me ", "echo that ", "back."],
        )
    })
}

/// The name of the shell tool, as `tetanus-exec` registers it. Named here as a
/// string because the loop's crate cannot depend on the crate that provides
/// it; a rename that missed this would show up as the shell path going
/// unexercised offline, which TC-CLI-SHELL-1 is watching for.
const SHELL: &str = "shell";

/// A step with no tool to call still owes an answer.
async fn answer_now(asked: &str, sink: &mut dyn ChunkSink) -> Result<ModelResponse, LlmError> {
    let content = format!("You said: {asked}");
    sink.chunk(StreamChunk::Text {
        delta: content.clone(),
    })
    .await?;
    Ok(ModelResponse {
        content: content.clone(),
        reasoning: String::new(),
        tool_calls: Vec::new(),
        finish_reason: "stop".into(),
        usage: Some(Usage {
            prompt_tokens: (asked.len() / 4) as u64,
            completion_tokens: (content.len() / 4) as u64,
        }),
    })
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
