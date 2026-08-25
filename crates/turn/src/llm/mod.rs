//! The LLM capability seam: the message and stream vocabulary plus the adapter
//! interface every provider implements. Adding a provider means implementing
//! [`LlmAdapter`] and providing it as the `llm` service at boot; nothing in the
//! turn engine names a provider.

pub mod attribution;
pub mod deepseek;
pub mod mock;
pub mod openai_compat;
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

/// The finish reasons that mean "the model reached the cap on what it may
/// write", in the words the providers use for it.
///
/// It is a list because the field is the provider's own word and not a
/// normalized one: the OpenAI-compatible wire, which the DeepSeek route
/// speaks, says `length`, and the other spelling is what a provider that
/// names the setting rather than the effect writes. Upstream normalizes
/// every provider to one `max-tokens` kind in its adapters; tetanus keeps the
/// provider's word on the journal, where it is evidence, and judges it here.
pub const TRUNCATED_FINISH_REASONS: [&str; 3] = ["length", "max_tokens", "max-tokens"];

impl ModelResponse {
    /// Whether the model stopped because it ran out of room rather than
    /// because it had finished (`docs/interface-contract.md` section 4.4.2).
    ///
    /// What follows from it is a turn that ends `max-tokens` and a step that
    /// dispatches none of the tool calls it carries: a completion that stopped
    /// mid-write can have stopped in the middle of a call's arguments.
    pub fn truncated(&self) -> bool {
        TRUNCATED_FINISH_REASONS.contains(&self.finish_reason.as_str())
    }
}

/// A failure the account cannot spend its way out of: the quota, balance or
/// credit is gone.
///
/// Deliberately outside
/// [`DEFAULT_RETRYABLE_CODES`](crate::llm::retry::DEFAULT_RETRYABLE_CODES). It
/// arrives as a 429, which is the same status a provider uses for "slow down",
/// and the two need opposite answers: asking again after a backoff is right
/// for a rate limit and pointless for an empty account. Before this
/// distinction existed a dead key spent the whole retry budget - every attempt,
/// every wait - and then reported `RATE_LIMIT`, which sends the reader looking
/// for a throughput problem instead of a billing one.
pub const QUOTA: &str = "QUOTA";

/// A request the model cannot accept because it is too big for its context.
///
/// Also terminal, and for a plainer reason: the same request will not fit the
/// next time either. What fixes it is sending less - compaction, or a shorter
/// history - and that is a decision above this seam, so the failure says what
/// happened rather than being retried until the budget runs out.
pub const CONTEXT_WINDOW_EXCEEDED: &str = "CONTEXT_WINDOW_EXCEEDED";

/// Whether a provider's own words say the account is out of quota.
///
/// Upstream matches this with regular expressions
/// (`packages/llm/llm/src/error.ts`, `isQuotaExceededError`); this normalizes
/// first and then looks for the same phrases, which needs no dependency and
/// accepts the same wordings. Normalizing is what makes `insufficient_quota`,
/// `insufficient-quota` and `Insufficient Quota` one case rather than three.
///
/// The list is wording a provider chose, so it is matched generously in shape
/// and narrowly in meaning: every phrase here says the balance is gone, and
/// none of them could be said by a provider that merely wants a slower caller.
pub fn names_exhausted_quota(detail: &str) -> bool {
    let text = normalized(detail);

    // "insufficient quota", "insufficient balance", "insufficient credits".
    for what in RESOURCES {
        if near(&text, "insufficient", what, 1) {
            return true;
        }
    }

    // The same fact as a sentence: "quota exceeded", "your balance is
    // exhausted", "you exceeded your current quota". A short window, because
    // a message that mentions the two far apart is probably discussing two
    // things, and the cost of guessing wrong here is a turn that fails when a
    // backoff would have fixed it.
    for what in RESOURCES.iter().chain(ALLOWANCES) {
        for spent in ["exceeded", "exhausted", "reached", "depleted"] {
            if near(&text, what, spent, 3) || near(&text, spent, what, 4) {
                return true;
            }
        }
    }

    ["credit", "credits", "budget", "money"]
        .iter()
        .any(|what| near(&text, "out of", what, 1))
}

/// What a provider says has run out. Deliberately not "limit": a rate limit is
/// a limit, and reading one as an empty account would fail a turn that a
/// backoff would have fixed.
const RESOURCES: &[&str] = &["quota", "balance", "credit", "credits"];

/// The named allowances that mean the account rather than the request rate.
const ALLOWANCES: &[&str] = &["usage limit", "monthly limit", "spending limit"];

/// Whether a provider's own words say the request was too big for the model's
/// context.
///
/// Ported from upstream's `isContextWindowExceededError`, by the same
/// normalize-then-match route.
pub fn names_context_overflow(detail: &str) -> bool {
    let text = normalized(detail);
    const OVERFLOW: [&str; 4] = [
        "context length exceeded",
        "context window exceeded",
        "context length overflow",
        "context limit exceeded",
    ];
    if OVERFLOW.iter().any(|phrase| text.contains(phrase)) {
        return true;
    }
    if near(&text, "maximum context", "length", 2) || near(&text, "max context", "length", 2) {
        return true;
    }
    // "prompt is too long for this model", "request too large for the context
    // window". The subject has to be the thing that was sent, so a provider
    // saying its own queue is too long is not read as this.
    for subject in ["request", "prompt", "input", "message", "messages"] {
        for size in ["too long", "too large"] {
            if near(&text, subject, size, 3)
                && (text.contains("context") || text.contains("for this model"))
            {
                return true;
            }
        }
        if near(&text, subject, "exceeds", 3) && text.contains("context") {
            return true;
        }
        if near(&text, subject, "exceeded", 3) && text.contains("context") {
            return true;
        }
    }
    false
}

/// Lowercase words, with everything that is not a letter or a digit read as a
/// separator.
///
/// Providers spell the same fact as `insufficient_quota`, `Insufficient
/// Quota` and `insufficient-quota`, and a classifier that treated those as
/// three different messages would work for whichever one it was written
/// against. Punctuation is a separator for the same reason and one more: a
/// message ends in a full stop, so `exceeded.` and `exceeded` are the same
/// word to a reader and have to be the same word here.
fn normalized(detail: &str) -> String {
    let spaced: String = detail
        .chars()
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
            _ => ' ',
        })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `first` is followed by `second` within `gap` words.
///
/// Both may be phrases. The window is what lets "exceeded your current quota"
/// match while keeping a sentence that mentions the two words far apart from
/// counting.
fn near(text: &str, first: &str, second: &str, gap: usize) -> bool {
    let words: Vec<&str> = text.split(' ').collect();
    let first: Vec<&str> = first.split(' ').collect();
    let second: Vec<&str> = second.split(' ').collect();

    for start in 0..words.len() {
        if !words[start..].starts_with(&first[..]) {
            continue;
        }
        let after = start + first.len();
        let limit = (after + gap + second.len()).min(words.len());
        for at in after..limit {
            if words[at..].starts_with(&second[..]) {
                return true;
            }
        }
    }
    false
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
    Provider {
        status: u16,
        message: String,
        /// The wait the provider asked for in its `Retry-After` header, in
        /// milliseconds, when it asked for one that can be honoured.
        ///
        /// `None` is "the provider asked for nothing", not "wait for no time":
        /// a policy that reads it falls back to its own backoff.
        retry_after_ms: Option<f64>,
        /// The provider's own id for the request that was refused.
        ///
        /// It is the only thing a user can quote to a provider's support, and
        /// it is the one fact about a refusal this harness cannot reconstruct:
        /// the status is on the response, the message is in the body, and the
        /// id exists only in the provider's logs. Discarding it makes "my
        /// request failed and nobody can tell me why" unanswerable.
        ///
        /// `None` is "the provider named none", which is common and not a
        /// fault: an error delivered inside an already-200 stream has no
        /// header left to carry one.
        request_id: Option<String>,
    },
    #[error("PROTOCOL: {0}")]
    Protocol(String),
    /// A provider that completed normally and said nothing at all.
    ///
    /// It is a failure rather than an empty answer, and a retryable one: the
    /// request was well formed, the model just produced no output, and asking
    /// again is the thing most likely to help. `EMPTY_RESPONSE` has been in
    /// [`retry::DEFAULT_RETRYABLE_CODES`](crate::llm::retry::DEFAULT_RETRYABLE_CODES)
    /// since the policy was ported; this is the error that reaches it.
    #[error("EMPTY_RESPONSE: {0}")]
    EmptyResponse(String),
    /// A provider that stopped speaking: the connection stayed open, and
    /// nothing arrived on it for longer than the adapter's idle window.
    ///
    /// It is its own failure and not a transport one. Nothing was refused and
    /// nothing was lost - the service simply went quiet - and the reader's
    /// next move is the one `TIMEOUT` names, which is the code upstream
    /// reports for the same silence
    /// (`packages/llm/llm-deepseek/src/adapter.ts`).
    #[error("TIMEOUT: {0}")]
    Timeout(String),
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
            LlmError::EmptyResponse(_) => "EMPTY_RESPONSE",
            LlmError::Timeout(_) => "TIMEOUT",
            // What the provider said is read before the status it said it
            // under, because the status is the coarser fact. A 429 is the
            // same number whether the account is out of money or merely
            // going too fast, and those need opposite answers.
            LlmError::Provider {
                status, message, ..
            } => match (status, message) {
                (_, detail) if names_exhausted_quota(detail) => QUOTA,
                (_, detail) if names_context_overflow(detail) => CONTEXT_WINDOW_EXCEEDED,
                (408, _) => "TIMEOUT",
                (429, _) => "RATE_LIMIT",
                (500..=599, _) => "SERVER",
                _ => "PROVIDER",
            },
            LlmError::Protocol(_) => "PROTOCOL",
            LlmError::Sink(_) => "SINK",
        }
    }

    /// The wait the provider asked for, in milliseconds, when this failure
    /// carries one.
    ///
    /// Only a provider response can ask: a transport that never reached the
    /// service has no header to ask in.
    pub fn retry_after_ms(&self) -> Option<f64> {
        match self {
            LlmError::Provider { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
    }

    /// The provider's id for the refused request, when it named one.
    ///
    /// An accessor rather than a match, so a caller that wants the id does not
    /// have to know which variants can carry one - the shape
    /// [`LlmError::retry_after_ms`] already has.
    pub fn request_id(&self) -> Option<&str> {
        match self {
            LlmError::Provider { request_id, .. } => request_id.as_deref(),
            _ => None,
        }
    }
}

/// The response headers a provider names its request id in, lowercased and
/// most specific first.
///
/// A list because there is no standard header and each provider picked its
/// own. The first two are the pair upstream's DeepSeek adapter reads
/// (`packages/llm/llm-deepseek/src/adapter.ts`); `request-id` is the same
/// header without the `x-` that predates it.
///
/// `cf-ray` is last and is deliberately in the list: a refusal generated at a
/// CDN edge never reached the provider, so it carries none of the others, and
/// the ray id is then the only identifier that exists for it. It is the least
/// specific answer, which is exactly why it is the last one tried.
///
/// Adding a provider means adding its spelling here, rather than teaching a
/// second place in the workspace about headers.
pub const REQUEST_ID_HEADERS: [&str; 5] = [
    "x-request-id",
    "x-deepseek-request-id",
    "request-id",
    "x-amzn-requestid",
    "cf-ray",
];

/// The request id a set of response headers carries, if any.
///
/// Takes a lookup rather than a header map, so the rule can be stated and
/// tested without a transport: what it is about is the preference order and
/// the trimming, and neither needs a socket to be got wrong.
pub fn request_id_from<'a>(header: impl Fn(&str) -> Option<&'a str>) -> Option<String> {
    REQUEST_ID_HEADERS
        .iter()
        .filter_map(|name| header(name))
        .map(str::trim)
        // A header present and empty is a provider that named nothing, which
        // is the same fact as sending none - and an empty string quoted to
        // support is worse than saying there was no id. The search continues
        // past it rather than stopping: a blank `x-request-id` beside a real
        // `cf-ray` is one usable id, and answering `None` would throw it away.
        .find(|value| !value.is_empty())
        .map(str::to_string)
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
