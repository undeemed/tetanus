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
use std::time::{Duration, Instant};

use futures_util::{Stream, StreamExt};

use crate::llm::attribution::{attribution_headers, AppIdentity};
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

/// What a stream that ends without the `[DONE]` sentinel is reported as.
///
/// The provider closes every complete stream with `[DONE]`. A stream that ends
/// without it was cut short: what arrived is not a short answer, it is an
/// unfinished one, and returning it would end the turn on half a message.
///
/// It is a `PROTOCOL` failure and not a `TRANSPORT` one, which keeps it outside
/// the default retryable set. The attempt already streamed chunks into the sink
/// and the journal, so repeating it is not free the way a refused connection
/// is. Upstream keeps its own `STREAM_CLOSED` out of the same set for the same
/// reason.
pub const STREAM_CLOSED: &str = "the stream ended without the [DONE] sentinel";

/// How long the provider may say nothing before its stream is given up on.
///
/// This is silence on the connection, not the life of the stream: an answer
/// that keeps arriving never times out however long it runs, and a provider
/// that stops speaking is a failure rather than a turn that never ends.
/// Upstream ships the same five minutes
/// (`packages/llm/llm-deepseek/src/adapter.ts`,
/// `DEFAULT_STREAM_IDLE_TIMEOUT_MS`).
pub const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;

/// How long one request may take in total, however busy the connection is.
///
/// The idle window catches a provider that goes quiet; this catches one that
/// never does. A route that answers a byte every four minutes for ever resets
/// the idle window on each byte and keeps a turn alive indefinitely, which is
/// the same unbounded turn the idle window was added to prevent, reached the
/// other way round.
///
/// Ten minutes is chosen to be longer than any legitimate single completion,
/// extended reasoning included, and short enough that a wedged route fails
/// while someone is still watching. Upstream has no equivalent - the idle
/// window is the only bound it ships - so this is the `llm/*` gap section 3
/// names rather than a port.
pub const DEFAULT_REQUEST_DEADLINE_MS: u64 = 600_000;

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
    /// The idle window this route's transport runs with, in milliseconds.
    ///
    /// Upstream refuses a window of zero where it reads its configuration.
    /// This adapter has no fallible constructor to refuse it in, so
    /// [`DeepSeekConfig::idle_window`] reads a zero as the default rather than
    /// as a window of no time, which would fail every request.
    pub stream_idle_timeout_ms: u64,
    /// The whole-request deadline this route runs with, in milliseconds.
    ///
    /// Read the same way as the idle window, and for the same reason: a zero
    /// is the default rather than a deadline of no time, which would fail
    /// every request. A deployment that wants no practical bound sets a large
    /// value rather than zero, so "unset" and "unbounded" stay different
    /// things.
    pub request_deadline_ms: u64,
}

impl DeepSeekConfig {
    /// The window a connection may stay silent for, with a zero read as
    /// [`DEFAULT_STREAM_IDLE_TIMEOUT_MS`].
    pub fn idle_window(&self) -> Duration {
        Duration::from_millis(match self.stream_idle_timeout_ms {
            0 => DEFAULT_STREAM_IDLE_TIMEOUT_MS,
            ms => ms,
        })
    }

    /// The whole-request budget, with a zero read as
    /// [`DEFAULT_REQUEST_DEADLINE_MS`].
    pub fn deadline(&self) -> Duration {
        Duration::from_millis(match self.request_deadline_ms {
            0 => DEFAULT_REQUEST_DEADLINE_MS,
            ms => ms,
        })
    }
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
            stream_idle_timeout_ms: DEFAULT_STREAM_IDLE_TIMEOUT_MS,
            request_deadline_ms: DEFAULT_REQUEST_DEADLINE_MS,
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

    /// The production wiring: a real HTTPS call, watched by the configured
    /// idle window.
    pub fn with_http(config: DeepSeekConfig) -> Self {
        let transport = ReqwestTransport::new(config.idle_window(), config.deadline());
        Self::new(config, Arc::new(transport))
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
        if !decoder.saw_done {
            return Err(LlmError::Protocol(STREAM_CLOSED.to_string()));
        }
        let (chunks, response) = decoder.finish();
        if is_empty_completion(&response) {
            return Err(LlmError::EmptyResponse(format!(
                "model {:?} returned a completed response with no content",
                request.model
            )));
        }
        for chunk in chunks {
            sink.chunk(chunk).await?;
        }
        Ok(response)
    }
}

/// Whether a completion that ended normally carried nothing at all.
///
/// Reasoning counts as content, and so does a tool call: a model that thought
/// out loud, or that answered by asking for a tool, produced output. What this
/// catches is the degenerate completion - a clean `stop` with no text, no
/// reasoning and no calls - which upstream classifies the same way
/// (`packages/llm/llm-pi-ai/src/stream.ts`, "a terminal stop that produced no
/// content blocks is a degenerate provider completion, not a successful (empty)
/// assistant message").
///
/// Only a `stop` is judged. A stream cut short is already
/// [`LlmError::Protocol`], and any other finish reason said something about why
/// it stopped, which is not this.
fn is_empty_completion(response: &ModelResponse) -> bool {
    response.finish_reason == DEFAULT_FINISH_REASON
        && response.content.is_empty()
        && response.reasoning.is_empty()
        && response.tool_calls.is_empty()
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

/// The header a provider asks for a wait in (RFC 9110 section 10.2.3).
pub const RETRY_AFTER_HEADER: &str = "retry-after";

/// The month names an IMF-fixdate is written with, in calendar order.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The wait a `Retry-After` header asks for, in milliseconds, or `None` when
/// it asks for nothing that can be honoured.
///
/// `now_ms` is the epoch time the date form is measured against. It is a
/// parameter rather than a clock read so the judgement is a function of its
/// inputs, and a case can state both sides of "in the past".
///
/// Both forms RFC 9110 defines are read: delta-seconds, and an IMF-fixdate.
/// A value that is zero, negative, already past, unreadable, or larger than
/// the seconds field can hold is not a wait, so it reads as nothing asked and
/// the policy is left on its own backoff. Refusing an uninterpretable value is
/// always safe; obeying one is not.
///
/// This lives on the one adapter with a response to read it from, as
/// [`normalize_api_key`] lives on the one adapter that resolves a credential.
/// It moves to a shared seam when a second adapter needs it.
pub fn retry_after_ms(header: &str, now_ms: f64) -> Option<f64> {
    let header = header.trim();
    if let Ok(seconds) = header.parse::<u32>() {
        return wait(f64::from(seconds) * 1000.0);
    }
    wait(imf_fixdate_ms(header)? - now_ms)
}

/// A wait is a positive, finite number of milliseconds. Everything else is a
/// provider that asked for nothing.
fn wait(ms: f64) -> Option<f64> {
    Some(ms).filter(|ms| ms.is_finite() && *ms > 0.0)
}

/// The epoch milliseconds an IMF-fixdate names, for example
/// `Sun, 06 Nov 1994 08:49:37 GMT`.
///
/// The two obsolete date formats RFC 9110 permits a recipient to accept are
/// not read. No provider sends them, and a date this cannot read is a wait
/// nobody asked for rather than a failure.
fn imf_fixdate_ms(text: &str) -> Option<f64> {
    let (_day_name, date) = text.strip_suffix(" GMT")?.split_once(", ")?;
    let mut fields = date.split(' ');
    let day: i64 = fields.next()?.parse().ok()?;
    let month_name = fields.next()?;
    let month = MONTHS.iter().position(|name| *name == month_name)? as i64 + 1;
    let year: i64 = fields.next()?.parse().ok()?;
    let mut clock = fields.next()?.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if fields.next().is_some() || clock.next().is_some() {
        return None;
    }
    // A leap second is minute 59 second 60, and it is a real value to read.
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(seconds as f64 * 1000.0)
}

/// Days from the epoch to a proleptic-Gregorian date, by Howard Hinnant's
/// `days_from_civil`. Calendar arithmetic is written once, from a published
/// algorithm, rather than approximated.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Epoch milliseconds now, for measuring a date the provider sent against.
fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |since| since.as_millis() as f64)
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
    /// Whether the terminating `[DONE]` sentinel arrived. A stream that ends
    /// without it ended early, whatever it managed to say first.
    saw_done: bool,
}

impl StreamDecoder {
    /// Feed one SSE `data:` payload and get the chunks it produced.
    ///
    /// `[DONE]` ends the answer. A payload after it is not part of the answer,
    /// so it decodes to nothing rather than appending to a message the provider
    /// already finished.
    pub fn push(&mut self, data: &str) -> Result<Vec<StreamChunk>, LlmError> {
        let data = data.trim();
        if data == "[DONE]" {
            self.saw_done = true;
            return Ok(Vec::new());
        }
        if self.saw_done || data.is_empty() {
            return Ok(Vec::new());
        }
        let frame: serde_json::Value = serde_json::from_str(data)
            .map_err(|e| LlmError::Protocol(format!("frame is not JSON: {e}")))?;

        if let Some(error) = frame.get("error") {
            return Err(LlmError::Provider {
                status: 200,
                message: error.to_string(),
                // An in-band error arrives inside a stream that already
                // answered 200, so there is no header left to ask in.
                retry_after_ms: None,
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

/// The production transport: one HTTPS request per `stream()` call, watched by
/// an idle window.
///
/// The watchdog sits on the connection and not around the decoded frames,
/// because a keep-alive comment carries no frame: the provider sends `:` lines
/// while a model thinks, and a window measured in frames would cut a stream
/// that is alive. `read_timeout` is armed before the response head and rearmed
/// on every read of the body, so what it measures is silence, and it covers a
/// service that accepts a connection and then never answers at all.
pub struct ReqwestTransport {
    client: reqwest::Client,
    /// Kept so a failure can name the window it exceeded.
    idle: Duration,
    /// Kept for the same reason, and to tell the two timeouts apart.
    deadline: Duration,
}

impl ReqwestTransport {
    /// A transport bounded twice: silent for at most `idle`, and running for
    /// at most `deadline` however talkative it is.
    ///
    /// Both bounds are needed and neither implies the other. The idle window
    /// catches a provider that stops speaking; the deadline catches one that
    /// never stops. A route that answers a byte every four minutes for ever
    /// resets the idle window on each byte, so without the deadline it is a
    /// turn that never ends.
    pub fn new(idle: Duration, deadline: Duration) -> Self {
        Self {
            client: reqwest::Client::builder()
                .read_timeout(idle)
                // `timeout` is the whole request, head and body together,
                // which is exactly the budget wanted here.
                .timeout(deadline)
                // The same failure `reqwest::Client::default()` panics on: a
                // build whose TLS backend cannot start has no transport to
                // hand back and no request it could serve.
                .build()
                .expect("a reqwest client"),
            idle,
            deadline,
        }
    }

    /// What a timeout is reported as, naming whichever bound was reached.
    ///
    /// The transport reports both as [`LlmError::Timeout`], whose code
    /// `TIMEOUT` is in
    /// [`retry::DEFAULT_RETRYABLE_CODES`](crate::llm::retry::DEFAULT_RETRYABLE_CODES),
    /// so either way the request is asked again rather than ending the turn.
    /// Only the message differs, and it differs because the two say different
    /// things about the route: one is quiet, the other is slow, and a reader
    /// deciding whether to raise the deadline needs to know which.
    ///
    /// Which bound was reached is decided by how long the request actually
    /// ran, because the transport reports both through one error kind. A
    /// request that has been running for its whole budget reached the
    /// deadline; anything sooner is the idle window.
    fn timeout_message(&self, url: &str, started: Instant) -> String {
        if started.elapsed() >= self.deadline {
            format!(
                "the request to {url} exceeded its {}ms deadline",
                self.deadline.as_millis()
            )
        } else {
            format!(
                "the stream from {url} was idle for {}ms",
                self.idle.as_millis()
            )
        }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new(
            Duration::from_millis(DEFAULT_STREAM_IDLE_TIMEOUT_MS),
            Duration::from_millis(DEFAULT_REQUEST_DEADLINE_MS),
        )
    }
}

#[async_trait::async_trait]
impl SseTransport for ReqwestTransport {
    async fn post_sse(
        &self,
        url: &str,
        api_key: &str,
        body: serde_json::Value,
    ) -> Result<FrameStream, LlmError> {
        // Every request says which product sent it, so a provider can tell
        // one client from another (`llm::attribution`).
        let mut request = self
            .client
            .post(url)
            .bearer_auth(api_key)
            .header("accept", "text/event-stream");
        for (name, value) in attribution_headers(&AppIdentity::default()) {
            request = request.header(name, value);
        }
        // When the request started, so a timeout can say which of the two
        // bounds it reached.
        let started = Instant::now();
        let response = request.json(&body).send().await.map_err(|e| {
            if e.is_timeout() {
                LlmError::Timeout(self.timeout_message(url, started))
            } else {
                LlmError::Transport(format!("request to {url} failed: {e}"))
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            // Read before the body: taking the text consumes the response,
            // and the wait it asked for is in the headers.
            let asked = response
                .headers()
                .get(RETRY_AFTER_HEADER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| retry_after_ms(value, now_ms()));
            let message = response.text().await.unwrap_or_default();
            return Err(LlmError::Provider {
                status: status.as_u16(),
                message,
                retry_after_ms: asked,
            });
        }

        // Worded here and moved into the stream: the frames outlive this call,
        // so they cannot borrow the transport to ask it later.
        // Worded lazily rather than eagerly: which bound a later failure
        // reached depends on how long the stream has been running by then,
        // which is not known yet.
        let (url_owned, idle, deadline) = (url.to_string(), self.idle, self.deadline);
        let stalled = move || {
            if started.elapsed() >= deadline {
                format!(
                    "the request to {url_owned} exceeded its {}ms deadline",
                    deadline.as_millis()
                )
            } else {
                format!(
                    "the stream from {url_owned} was idle for {}ms",
                    idle.as_millis()
                )
            }
        };
        let bytes = response.bytes_stream().map(move |chunk| {
            chunk.map(|b| b.to_vec()).map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout(stalled())
                } else {
                    LlmError::Transport(e.to_string())
                }
            })
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
