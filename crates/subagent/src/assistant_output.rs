//! What a child agent actually answered.
//!
//! When a delegated run ends, the parent needs one thing back: the child's
//! final answer. A journal holds several candidates — every assistant message
//! the child wrote, and the raw stream chunks underneath them — and picking
//! the wrong one is how a parent ends up reporting an empty result for a run
//! that plainly said something.
//!
//! The rule, in order:
//!
//! 1. The **last non-empty** assistant message.
//! 2. Failing that, the accumulated streamed text.
//! 3. Failing that, nothing — the child genuinely produced no answer.
//!
//! Two details carry the weight. *Last* rather than first, because a child
//! that kept working after an intermediate answer meant the later one. And
//! *non-empty*, because the turn loop appends an empty message to record usage
//! after a step that produced no visible output; letting that empty message
//! win would erase a real answer written moments earlier.
//!
//! Selection does not consult the stop reason. A child that was interrupted
//! still said what it said, and the parent decides what to do about the
//! reason separately.
//!
//! # Adapted to this journal's vocabulary
//!
//! Upstream folds `ContentBlock[]`. Here an `assistant/message` carries its
//! content as a string and an `assistant/chunk` is a tagged stream chunk, so
//! the fold reads those instead. The rule is unchanged; only what it reads is.
//!
//! Parity: upstream `packages/subagent/subagent/src/assistant-output.ts`,
//! pinned by its `assistant-output.spec.ts`.

use serde_json::Value;
use tetanus_session::SessionEvent;

/// The fold, for a caller watching a child's journal as it grows.
///
/// Incremental because a backend may observe a child over a transport that
/// hands it pieces, and re-scanning the whole journal per piece is the wrong
/// shape for something that runs on every chunk.
#[derive(Debug, Clone, Default)]
pub struct AssistantOutputFold {
    /// The best complete answer seen so far.
    message: Option<String>,
    /// Streamed text, kept only as the fallback.
    partial: String,
}

impl AssistantOutputFold {
    /// A fold that has seen nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer one journal record. Anything that is not an assistant message or
    /// a text chunk contributes nothing.
    pub fn push(&mut self, event: &SessionEvent) {
        match event.ty.as_str() {
            "assistant/message" => {
                let content = event.data.get("content").and_then(Value::as_str);
                // Empty content is the usage-only message the loop appends; it
                // must not displace a real answer.
                if let Some(content) = content.filter(|text| !text.is_empty()) {
                    self.message = Some(content.to_owned());
                }
            }
            // Only the visible text channel. Reasoning is model-visible but is
            // not the answer, and a fold that took it would report a child's
            // thinking as its result.
            "assistant/chunk"
                if event.data.get("chunk").and_then(Value::as_str) == Some("text") =>
            {
                if let Some(delta) = event.data.get("delta").and_then(Value::as_str) {
                    self.push_text(delta);
                }
            }
            _ => {}
        }
    }

    /// Offer streamed text observed outside the journal.
    ///
    /// A transport that carries content without journal records still needs
    /// the fallback, and this is the way in. An empty piece is not a piece.
    pub fn push_text(&mut self, text: &str) {
        if !text.is_empty() {
            self.partial.push_str(text);
        }
    }

    /// The answer as it stands.
    ///
    /// Borrowing rather than consuming, because a caller watching a live child
    /// may ask more than once.
    pub fn collect(&self) -> Option<String> {
        if let Some(message) = &self.message {
            return Some(message.clone());
        }
        if self.partial.is_empty() {
            return None;
        }
        Some(self.partial.clone())
    }
}

/// The answer in a finished run's records.
pub fn final_assistant_output(events: &[SessionEvent]) -> Option<String> {
    let mut fold = AssistantOutputFold::new();
    for event in events {
        fold.push(event);
    }
    fold.collect()
}
