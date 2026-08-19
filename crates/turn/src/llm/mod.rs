//! The LLM capability seam: the message and stream vocabulary plus the adapter
//! interface every provider implements. Adding a provider means implementing
//! [`LlmAdapter`] and providing it as the `llm` service at boot; nothing in the
//! turn engine names a provider.

pub mod deepseek;
pub mod mock;
pub mod retry;

use crate::tools::{ToolCall, ToolSchema};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// One model-visible message. Every field here is reconstructable from the
/// session log; nothing reaches a request that the log does not record.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Set on a `tool` message: the call this result answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
        }
    }
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// A fully assembled model request. The engine builds it, the `agent/request`
/// waterfall may rewrite it, and the adapter serializes it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelRequest {
    pub provider: String,
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

/// One incremental piece of a provider stream.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "chunk", rename_all = "snake_case")]
pub enum StreamChunk {
    /// Visible assistant text.
    Text { delta: String },
    /// Thinking-mode text; model-visible, kept out of derived history.
    Reasoning { delta: String },
    /// A tool call the provider finished assembling.
    ToolCall { call: ToolCall },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// The outcome of one provider call. Recorded verbatim as `assistant/message`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelResponse {
    pub content: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("MISSING_CREDENTIAL: no API key at {0}")]
    MissingCredential(String),
    #[error("INVALID_CREDENTIAL: the key at {0} cannot be carried in an HTTP header")]
    InvalidCredential(String),
    #[error("TRANSPORT: {0}")]
    Transport(String),
    #[error("PROVIDER: {status} {message}")]
    Provider { status: u16, message: String },
    #[error("PROTOCOL: {0}")]
    Protocol(String),
    #[error("SINK: {0}")]
    Sink(String),
}

impl LlmError {
    /// The stable failure code a policy is written against, such as
    /// [`retry::RetryPolicy`].
    ///
    /// For everything but a provider response it is the prefix this error
    /// already prints. A provider response is classed by its status instead,
    /// because "the service is overloaded" and "the request was malformed"
    /// arrive the same way and are not the same failure.
    pub fn code(&self) -> &'static str {
        match self {
            LlmError::MissingCredential(_) => "MISSING_CREDENTIAL",
            LlmError::InvalidCredential(_) => "INVALID_CREDENTIAL",
            LlmError::Transport(_) => "TRANSPORT",
            LlmError::Provider { status, .. } => match status {
                408 => "TIMEOUT",
                429 => "RATE_LIMIT",
                500..=599 => "SERVER",
                _ => "PROVIDER",
            },
            LlmError::Protocol(_) => "PROTOCOL",
            LlmError::Sink(_) => "SINK",
        }
    }
}

/// Where an adapter delivers chunks as they arrive. The turn engine's sink
/// appends each one to the session log, so replay and UI fidelity survive.
#[async_trait::async_trait]
pub trait ChunkSink: Send + Sync {
    async fn chunk(&mut self, chunk: StreamChunk) -> Result<(), LlmError>;
}

/// A collecting sink, for adapter tests and for callers that only want the
/// final response.
#[derive(Debug, Default)]
pub struct CollectingSink {
    pub chunks: Vec<StreamChunk>,
}

#[async_trait::async_trait]
impl ChunkSink for CollectingSink {
    async fn chunk(&mut self, chunk: StreamChunk) -> Result<(), LlmError> {
        self.chunks.push(chunk);
        Ok(())
    }
}

/// One model provider.
#[async_trait::async_trait]
pub trait LlmAdapter: Send + Sync {
    /// The provider route this adapter owns, e.g. `deepseek-official`.
    fn provider(&self) -> &str;
    /// Advisory model catalog; an unlisted model id still passes through.
    fn models(&self) -> Vec<String> {
        Vec::new()
    }
    /// Environment variable holding this provider's credential, when it needs
    /// one. `None` for an adapter that runs with no key, such as the mock, so
    /// a catalog can report which providers are usable without asking each
    /// one to fail first.
    fn credential_env(&self) -> Option<&str> {
        None
    }
    /// Make exactly one provider request, streaming chunks into `sink`.
    async fn stream(
        &self,
        request: &ModelRequest,
        sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError>;
}
