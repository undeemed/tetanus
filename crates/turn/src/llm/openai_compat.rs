//! Any endpoint that speaks the OpenAI chat-completions protocol, under a
//! route the deployment names.
//!
//! [`crate::llm::deepseek`] already *is* that transport: `POST
//! {base_url}/chat/completions`, an SSE stream, a bearer credential read per
//! request from the configured environment variable. The one part of it that
//! is not generic is the route it answers to, which is the constant
//! `deepseek-official`. So this is a rename and nothing else: the wrapped
//! adapter keeps the wire format, the timeouts, the retry-relevant error
//! classes and the request-id headers, and the wrapper answers
//! [`LlmAdapter::provider`] with the name a settings document wrote.
//!
//! Forking the adapter would have been the other way to do it, and it is the
//! reason there is a wrapper at all: two copies of one wire format drift, and
//! the copy that drifts is the one nobody has a live key for.
//!
//! Local servers with no credential (Ollama, vLLM, LM Studio) are served by
//! pointing `api_key_env` at any variable holding a placeholder - they ignore
//! the bearer header. There is deliberately no keyless mode in the transport:
//! a route that needs no key and a route whose key is missing would then be
//! the same state, and the second one has to fail.

use std::sync::Arc;

use crate::llm::deepseek::{DeepSeekAdapter, DeepSeekConfig, SseTransport};
use crate::llm::{ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse};

/// One OpenAI-compatible route, named by whoever configured it.
pub struct OpenAiCompatAdapter {
    /// The provider route this adapter answers to, as a document wrote it.
    route: String,
    inner: DeepSeekAdapter,
}

impl OpenAiCompatAdapter {
    /// The wiring a test uses: a named route over a transport it controls.
    pub fn new(route: String, config: DeepSeekConfig, transport: Arc<dyn SseTransport>) -> Self {
        Self {
            route,
            inner: DeepSeekAdapter::new(config, transport),
        }
    }

    /// The production wiring: a real HTTP call, watched by the two bounds the
    /// wrapped config carries.
    pub fn with_http(route: String, config: DeepSeekConfig) -> Self {
        Self {
            route,
            inner: DeepSeekAdapter::with_http(config),
        }
    }

    /// The route, for a composer that has the adapter and not the name it was
    /// built with.
    pub fn route(&self) -> &str {
        &self.route
    }

    /// What this route runs on, for a caller that resolved the block and wants
    /// to read back what it settled to.
    pub fn config(&self) -> &DeepSeekConfig {
        self.inner.config()
    }
}

#[async_trait::async_trait]
impl LlmAdapter for OpenAiCompatAdapter {
    fn provider(&self) -> &str {
        &self.route
    }

    fn models(&self) -> Vec<String> {
        self.inner.models()
    }

    fn credential_env(&self) -> Option<&str> {
        self.inner.credential_env()
    }

    async fn stream(
        &self,
        request: &ModelRequest,
        sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError> {
        self.inner.stream(request, sink).await
    }
}
