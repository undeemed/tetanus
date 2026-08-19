//! DeepSeek chat-completions adapter: the provider route upstream ships as its
//! default (`deepseek-official`). Wire format is the official
//! `POST {baseURL}/chat/completions` SSE stream, which is OpenAI-compatible, so
//! the same adapter serves any endpoint speaking that protocol.
//!
//! The HTTP call sits behind [`SseTransport`] so the wire serialization and the
//! stream decoder are exercised in tests without a network. A live call happens
//! only when the configured key env var is set.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::{Stream, StreamExt};

use crate::llm::{
    ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse, Role, StreamChunk, Usage,
};
use crate::tools::ToolCall;

pub const PROVIDER: &str = "deepseek-official";
pub const PUBLIC_BASE_URL: &str = "https://api.deepseek.com";
pub const DEFAULT_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
pub const BASE_URL_ENV: &str = "DEEPSEEK_BASE_URL";

/// What a tool message carries when the tool produced no output. Blank content
/// reads to the model as a tool that said nothing, which it cannot tell apart
/// from a tool that never ran.
pub const EMPTY_TOOL_RESULT: &str = "(no output)";

/// The finish reason a stream that never states one is reported as. The
/// provider omits the field on some plain completions, and an empty string
/// downstream reads as a turn that ended for no stated reason.
pub const DEFAULT_FINISH_REASON: &str = "stop";

#[derive(Debug, Clone)]
pub struct DeepSeekConfig {
    /// Credential reference: the environment variable holding the key. Config
    /// never carries a literal key.
    pub api_key_env: String,
    pub base_url: String,
    /// Advisory catalog; an unlisted model id still passes through unchanged.
    pub models: Vec<String>,
    /// Adapter-configured output cap; an explicit request value wins.
    pub max_tokens: Option<u32>,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            base_url: std::env::var(BASE_URL_ENV)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| PUBLIC_BASE_URL.to_string()),
            models: vec!["deepseek-v4-flash".into(), "deepseek-v4-pro".into()],
            max_tokens: None,
        }
    }
}

/// A stream of SSE `data:` payloads, in arrival order, `[DONE]` included.
pub type FrameStream = Pin<Box<dyn Stream<Item = Result<String, LlmError>> + Send>>;

/// The one network seam. Tests provide a replaying transport; production
/// provides [`ReqwestTransport`].
#[async_trait::async_trait]
pub trait SseTransport: Send + Sync {
    async fn post_sse(
        &self,
        url: &str,
        api_key: &str,
        body: serde_json::Value,
    ) -> Result<FrameStream, LlmError>;
}

pub struct DeepSeekAdapter {
    config: DeepSeekConfig,
    transport: Arc<dyn SseTransport>,
}

impl DeepSeekAdapter {
    pub fn new(config: DeepSeekConfig, transport: Arc<dyn SseTransport>) -> Self {
        Self { config, transport }
    }

    /// The production wiring: a real HTTPS call.
    pub fn with_http(config: DeepSeekConfig) -> Self {
        Self::new(config, Arc::new(ReqwestTransport::default()))
    }

    pub fn config(&self) -> &DeepSeekConfig {
        &self.config
    }

    /// Resolve the key from the trusted environment layer. Phase ② adds the
    /// credential seam that can answer before the environment does.
    fn api_key(&self) -> Result<String, LlmError> {
        let raw = std::env::var(&self.config.api_key_env).unwrap_or_default();
        normalize_api_key(&raw, &self.config.api_key_env)
    }
}

#[async_trait::async_trait]
impl LlmAdapter for DeepSeekAdapter {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn models(&self) -> Vec<String> {
        self.config.models.clone()
    }

    fn credential_env(&self) -> Option<&str> {
        Some(&self.config.api_key_env)
    }

    async fn stream(
        &self,
        request: &ModelRequest,
        sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError> {
        let api_key = self.api_key()?;
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let body = wire_request(request, self.config.max_tokens);

        let mut frames = self.transport.post_sse(&url, &api_key, body).await?;
        let mut decoder = StreamDecoder::default();
        while let Some(frame) = frames.next().await {
            for chunk in decoder.push(&frame?)? {
                sink.chunk(chunk).await?;
            }
        }
        let (chunks, response) = decoder.finish();
        for chunk in chunks {
            sink.chunk(chunk).await?;
        }
        Ok(response)
    }
}

/// Judge a stored credential and return the form that goes on the wire.
///
/// Trimming happens before judging, so a key pasted with a trailing newline
/// works and a key of nothing but spaces reads as absent rather than as
/// present and wrong. What survives has to be carriable in an `Authorization`
/// header verbatim, which is printable ASCII with no space: an interior space
/// is a second header token, and anything above ASCII is not header-safe at
/// all. The error names the configuration entry the key came from and never
/// any part of the key.
pub fn normalize_api_key(raw: &str, reference: &str) -> Result<String, LlmError> {
    let key = raw.trim();
    if key.is_empty() {
        return Err(LlmError::MissingCredential(reference.to_string()));
    }
    if !key.chars().all(|c| ('!'..='~').contains(&c)) {
        return Err(LlmError::InvalidCredential(reference.to_string()));
    }
    Ok(key.to_string())
}

/// Serialize a [`ModelRequest`] into the official wire body. Pure, so the
/// contract is testable without a transport.
pub fn wire_request(request: &ModelRequest, adapter_max_tokens: Option<u32>) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .map(|message| {
            let content = match (&message.role, message.content.as_str()) {
                (Role::Tool, "") => EMPTY_TOOL_RESULT,
                _ => message.content.as_str(),
            };
            let mut wire = serde_json::json!({
                "role": message.role.as_str(),
                "content": content,
            });
            let object = wire.as_object_mut().expect("object literal");
            if let Some(id) = &message.tool_call_id {
                object.insert("tool_call_id".into(), id.clone().into());
            }
            if !message.tool_calls.is_empty() {
                let calls: Vec<serde_json::Value> = message
                    .tool_calls
                    .iter()
                    .map(|call| {
                        serde_json::json!({
                            "id": call.id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                // Arguments cross the wire as a JSON string.
                                "arguments": call.arguments.to_string(),
                            },
                        })
                    })
                    .collect();
                object.insert("tool_calls".into(), calls.into());
            }
            wire
        })
        .collect();

    let mut body = serde_json::json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    let object = body.as_object_mut().expect("object literal");
    if !request.tools.is_empty() {
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    },
                })
            })
            .collect();
        object.insert("tools".into(), tools.into());
    }
    if let Some(max_tokens) = request.max_tokens.or(adapter_max_tokens) {
        object.insert("max_tokens".into(), max_tokens.into());
    }
    body
}

#[derive(Default, Debug)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

/// Translates the provider wire format into the `StreamChunk` protocol.
/// Fragmented `tool_calls` deltas are accumulated by index and become one
/// chunk each once the stream ends.
#[derive(Default, Debug)]
pub struct StreamDecoder {
    content: String,
    reasoning: String,
    calls: BTreeMap<u64, PartialCall>,
    finish_reason: String,
    usage: Option<Usage>,
}

impl StreamDecoder {
    /// Feed one SSE `data:` payload and get the chunks it produced.
    pub fn push(&mut self, data: &str) -> Result<Vec<StreamChunk>, LlmError> {
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(Vec::new());
        }
        let frame: serde_json::Value = serde_json::from_str(data)
            .map_err(|e| LlmError::Protocol(format!("frame is not JSON: {e}")))?;

        if let Some(error) = frame.get("error") {
            return Err(LlmError::Provider {
                status: 200,
                message: error.to_string(),
            });
        }

        if let Some(usage) = frame.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(Usage {
                prompt_tokens: u64_field(usage, "prompt_tokens"),
                completion_tokens: u64_field(usage, "completion_tokens"),
            });
        }

        let mut chunks = Vec::new();
        let choices = match frame.get("choices").and_then(serde_json::Value::as_array) {
            Some(choices) => choices,
            None => return Ok(chunks),
        };
        for choice in choices {
            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(serde_json::Value::as_str)
            {
                self.finish_reason = reason.to_string();
            }
            let delta = match choice.get("delta") {
                Some(delta) => delta,
                None => continue,
            };
            if let Some(text) = delta.get("content").and_then(serde_json::Value::as_str) {
                if !text.is_empty() {
                    self.content.push_str(text);
                    chunks.push(StreamChunk::Text {
                        delta: text.to_string(),
                    });
                }
            }
            if let Some(text) = delta
                .get("reasoning_content")
                .and_then(serde_json::Value::as_str)
            {
                if !text.is_empty() {
                    self.reasoning.push_str(text);
                    chunks.push(StreamChunk::Reasoning {
                        delta: text.to_string(),
                    });
                }
            }
            for call in delta
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let index = call
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let partial = self.calls.entry(index).or_default();
                if let Some(id) = call.get("id").and_then(serde_json::Value::as_str) {
                    partial.id = id.to_string();
                }
                if let Some(function) = call.get("function") {
                    if let Some(name) = function.get("name").and_then(serde_json::Value::as_str) {
                        partial.name.push_str(name);
                    }
                    if let Some(args) = function
                        .get("arguments")
                        .and_then(serde_json::Value::as_str)
                    {
                        partial.arguments.push_str(args);
                    }
                }
            }
        }
        Ok(chunks)
    }

    /// Close the stream: assemble the accumulated tool calls into their chunks
    /// and the final response.
    pub fn finish(self) -> (Vec<StreamChunk>, ModelResponse) {
        let mut chunks = Vec::new();
        let mut tool_calls = Vec::new();
        for (index, partial) in self.calls {
            let arguments = if partial.arguments.trim().is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(&partial.arguments)
                    .unwrap_or_else(|_| serde_json::Value::String(partial.arguments.clone()))
            };
            let call = ToolCall {
                id: if partial.id.is_empty() {
                    format!("call_{index}")
                } else {
                    partial.id.clone()
                },
                name: partial.name.clone(),
                arguments,
            };
            chunks.push(StreamChunk::ToolCall { call: call.clone() });
            tool_calls.push(call);
        }
        let response = ModelResponse {
            content: self.content,
            reasoning: self.reasoning,
            tool_calls,
            finish_reason: if self.finish_reason.is_empty() {
                DEFAULT_FINISH_REASON.to_string()
            } else {
                self.finish_reason
            },
            usage: self.usage,
        };
        (chunks, response)
    }
}

fn u64_field(value: &serde_json::Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// Split a raw SSE byte buffer into complete `data:` payloads, leaving any
/// partial event in `buffer`. Comment lines (`:`) are transport keep-alives and
/// never become frames.
pub fn take_frames(buffer: &mut String) -> Vec<String> {
    let mut frames = Vec::new();
    while let Some((index, width)) = buffer
        .find("\n\n")
        .map(|i| (i, 2))
        .or_else(|| buffer.find("\r\n\r\n").map(|i| (i, 4)))
    {
        let block: String = buffer.drain(..index + width).collect();
        let mut data = String::new();
        for line in block.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(rest) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.trim_start());
            }
        }
        if !data.is_empty() {
            frames.push(data);
        }
    }
    frames
}

/// The production transport: one HTTPS request per `stream()` call.
#[derive(Default)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

#[async_trait::async_trait]
impl SseTransport for ReqwestTransport {
    async fn post_sse(
        &self,
        url: &str,
        api_key: &str,
        body: serde_json::Value,
    ) -> Result<FrameStream, LlmError> {
        let response = self
            .client
            .post(url)
            .bearer_auth(api_key)
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transport(format!("request to {url} failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(LlmError::Provider {
                status: status.as_u16(),
                message,
            });
        }

        let bytes = response.bytes_stream().map(|chunk| {
            chunk
                .map(|b| b.to_vec())
                .map_err(|e| LlmError::Transport(e.to_string()))
        });

        let stream = futures_util::stream::unfold(
            (
                Box::pin(bytes),
                String::new(),
                std::collections::VecDeque::new(),
            ),
            |(mut body, mut buffer, mut pending)| async move {
                loop {
                    if let Some(frame) = pending.pop_front() {
                        return Some((Ok(frame), (body, buffer, pending)));
                    }
                    match body.next().await {
                        Some(Ok(bytes)) => {
                            buffer.push_str(&String::from_utf8_lossy(&bytes));
                            pending.extend(take_frames(&mut buffer));
                        }
                        Some(Err(e)) => return Some((Err(e), (body, buffer, pending))),
                        None => return None,
                    }
                }
            },
        );
        Ok(Box::pin(stream))
    }
}

/// A transport that replays canned frames, so adapter tests never touch a
/// network.
pub struct ReplayTransport {
    frames: Vec<String>,
    /// The body the adapter sent, captured for assertions.
    pub sent: std::sync::Mutex<Option<serde_json::Value>>,
}

impl ReplayTransport {
    pub fn new(frames: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            frames: frames.into_iter().map(Into::into).collect(),
            sent: std::sync::Mutex::new(None),
        }
    }

    /// The body of the last request, for assertions.
    pub fn last_body(&self) -> Option<serde_json::Value> {
        self.sent.lock().expect("sent lock").clone()
    }
}

#[async_trait::async_trait]
impl SseTransport for ReplayTransport {
    async fn post_sse(
        &self,
        _url: &str,
        _api_key: &str,
        body: serde_json::Value,
    ) -> Result<FrameStream, LlmError> {
        *self.sent.lock().expect("sent lock") = Some(body);
        let frames: Vec<Result<String, LlmError>> = self.frames.iter().cloned().map(Ok).collect();
        Ok(Box::pin(futures_util::stream::iter(frames)))
    }
}
