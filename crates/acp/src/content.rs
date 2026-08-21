//! Turning an ACP prompt into the one string a turn takes, and refusing the
//! prompts that cannot become one.
//!
//! Admission is all-or-nothing and happens before anything durable is written.
//! A prompt half-admitted - three blocks accepted, the fourth refused - would
//! leave a user message on the journal describing a turn that never ran, and
//! the journal is the session's whole history.

use tetanus_protocol::rpc::{ErrorCode, RpcError};

use crate::wire::ContentBlock;

/// What a rejected prompt failed on.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentError {
    pub message: String,
    /// The block kind at fault, when one block is.
    pub kind: Option<String>,
}

impl ContentError {
    fn new(message: impl Into<String>, kind: Option<&str>) -> Self {
        Self {
            message: message.into(),
            kind: kind.map(str::to_string),
        }
    }
}

impl From<ContentError> for RpcError {
    fn from(error: ContentError) -> Self {
        let wire = RpcError::new(ErrorCode::InvalidParams, error.message);
        match error.kind {
            Some(kind) => wire.with_data(serde_json::json!({ "kind": kind })),
            None => wire,
        }
    }
}

/// The whole prompt as one message, or the reason it is not a prompt.
///
/// Order is preserved and adjacent text is joined with a newline rather than
/// concatenated: two text blocks are two things the client chose to send
/// separately, and running them together can weld the end of one sentence to
/// the start of the next.
///
/// A resource link flattens to a bracketed reference rather than being fetched.
/// The link names something the model can open with its own tools if it has
/// them, and fetching it here would put a filesystem read inside prompt
/// admission, where no sandbox policy applies and no `fs/*` event records it.
///
/// Every other block kind is refused, each naming itself, because
/// `initialize` already said this bridge does not take them and a client that
/// sent one anyway needs to know which one.
pub fn admit(prompt: &[ContentBlock]) -> Result<String, ContentError> {
    if prompt.is_empty() {
        return Err(ContentError::new(
            "a prompt carries at least one block",
            None,
        ));
    }

    let mut parts: Vec<String> = Vec::new();
    for block in prompt {
        match block {
            ContentBlock::Text { text } => parts.push(text.clone()),
            ContentBlock::ResourceLink { name, uri } => {
                parts.push(format!("[resource_link name={name} uri={uri}]"))
            }
            ContentBlock::Image { .. } => {
                return Err(ContentError::new(
                    "this agent did not advertise the `image` prompt capability",
                    Some("image"),
                ))
            }
            ContentBlock::Audio { .. } => {
                return Err(ContentError::new(
                    "this agent did not advertise the `audio` prompt capability",
                    Some("audio"),
                ))
            }
            ContentBlock::Resource { .. } => {
                return Err(ContentError::new(
                    "this agent did not advertise the `embeddedContext` prompt capability",
                    Some("resource"),
                ))
            }
            ContentBlock::Other(_) => {
                return Err(ContentError::new(
                    format!("unsupported content block `{}`", block.kind()),
                    Some(block.kind()),
                ))
            }
        }
    }

    let admitted = parts.join("\n");
    if admitted.trim().is_empty() {
        // Blocks that are all whitespace are a prompt in shape and not in
        // substance. Running a turn on one spends a model request to answer
        // nothing, and the client cannot tell that is what it asked for.
        return Err(ContentError::new("a prompt carries some text", None));
    }
    Ok(admitted)
}
