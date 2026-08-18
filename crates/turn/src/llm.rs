use tokio::sync::mpsc;

/// LLM adapter seam: implementations stream deltas.
#[async_trait::async_trait]
pub trait LlmAdapter: Send + Sync {
    async fn stream(
        &self,
        prompt: String,
        tx: mpsc::Sender<String>,
    ) -> Result<(), LlmError>;
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("llm request failed: {0}")]
    Request(String),
}

/// Deterministic echo adapter for tests/conformance.
pub struct EchoAdapter;

#[async_trait::async_trait]
impl LlmAdapter for EchoAdapter {
    async fn stream(&self, prompt: String, tx: mpsc::Sender<String>) -> Result<(), LlmError> {
        for chunk in prompt.split_inclusive(' ') {
            let _ = tx.send(chunk.to_string()).await;
        }
        Ok(())
    }
}
