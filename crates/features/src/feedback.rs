//! The channel a run reports something back to the operator on.
//!
//! **Every entry is its own record.** Feedback is not state with a current
//! value: two remarks are two facts, and the second does not replace the first.
//! This is the one feature in this crate whose fold is a list rather than a
//! last-wins value, and the difference is the point - a todo list is what is
//! true now, and feedback is what was said when.
//!
//! **It never reaches the model.** `feedback/recorded` derives to no message,
//! so a remark about the run does not become part of the conversation the run
//! is having. That matters in both directions: a person writing "this is going
//! badly" is talking to the operator and not steering the model, and a model
//! reading its own operator feedback would be reasoning about a channel it is
//! not a party to.
//!
//! **Empty is refused, and whitespace is normalized rather than parsed.**
//! Surrounding whitespace is discarded and nothing else is touched: content
//! that looks like a command, a slash, or markup is a remark like any other.
//! A channel that interpreted what it was given would be a channel a person
//! cannot use to report the thing they are actually looking at.
//!
//! Parity: upstream `packages/feedback/command-feedback`, pinned by its
//! `command-feedback.spec.ts`. Upstream's `message-feedback` - a per-message
//! rating with its own durable store, versions and checkpoints - is a different
//! feature over a store this workspace does not have;
//! `docs/parity-updates/` names it.

use std::sync::Arc;

use serde_json::{json, Value};
use tetanus_session::{SessionError, SessionEvent, SessionLog};
use tetanus_turn::tools::{Tool, ToolError, ToolOutcome, ToolSchema};

/// The durable type this module writes.
pub mod topic {
    /// One remark about this session. Log-only: it never enters model context
    /// or derived history.
    pub const FEEDBACK_RECORDED: &str = "feedback/recorded";
}

/// One recorded remark.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    /// The remark, with surrounding whitespace discarded and nothing else
    /// changed.
    pub text: String,
    /// Who said it, when the caller knew. `None` is an unattributed remark
    /// rather than an anonymous one: the field says what was recorded, and a
    /// run that had no author to name should not invent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("feedback needs something to say: an empty remark records nothing")]
pub struct EmptyFeedback;

/// Every remark this journal carries, oldest first.
///
/// A list, deliberately, where the other features in this crate fold to one
/// value. Ordering is the journal's, which is the order they were recorded in,
/// so two remarks made in one turn read back in the order they were made.
pub fn recorded(events: &[SessionEvent]) -> Vec<Entry> {
    events
        .iter()
        .filter(|event| event.ty == topic::FEEDBACK_RECORDED)
        .filter_map(|event| serde_json::from_value::<Entry>(event.data.clone()).ok())
        .collect()
}

/// Record one remark.
///
/// The normalization is the whole of the processing: trim, refuse empty, store.
/// Nothing here reads the content, and a caller that wanted it parsed would be
/// asking for a different channel.
pub fn record(
    log: &dyn SessionLog,
    text: &str,
    author: Option<&str>,
) -> Result<Entry, RecordError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(EmptyFeedback.into());
    }
    let entry = Entry {
        text: text.to_string(),
        author: author
            .map(str::trim)
            .filter(|author| !author.is_empty())
            .map(str::to_string),
    };
    log.append(topic::FEEDBACK_RECORDED, json!(entry))?;
    Ok(entry)
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error(transparent)]
    Empty(#[from] EmptyFeedback),
    #[error(transparent)]
    Log(#[from] SessionError),
}

/// The model-facing tool.
///
/// A model uses it to tell the operator something the conversation is not the
/// place for: that an instruction was contradictory, that a tool it needed is
/// missing, that it is doing something it thinks is wrong. Registering it is
/// how a deployment says it is listening.
pub struct FeedbackTool {
    log: Arc<dyn SessionLog>,
}

impl FeedbackTool {
    pub const NAME: &'static str = "report_feedback";

    pub fn new(log: Arc<dyn SessionLog>) -> Arc<Self> {
        Arc::new(Self { log })
    }
}

#[async_trait::async_trait]
impl Tool for FeedbackTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Report something back to the operator of this run: a contradictory \
                          instruction, a missing capability, or a concern about the work. It is \
                          recorded for a person to read and does not become part of this \
                          conversation."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "What to report, in your own words.",
                    },
                },
                "required": ["text"],
            }),
        }
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let text = arguments.get("text").and_then(Value::as_str).unwrap_or("");
        match record(self.log.as_ref(), text, Some("model")) {
            Ok(entry) => Ok(ToolOutcome::ok(format!(
                "Recorded for the operator ({} characters). It is not part of the conversation, \
                 so continue with the work.",
                entry.text.chars().count()
            ))),
            Err(RecordError::Empty(empty)) => Ok(ToolOutcome::failed(empty.to_string())),
            Err(RecordError::Log(e)) => Err(ToolError::Failed(Self::NAME.into(), e.to_string())),
        }
    }
}
