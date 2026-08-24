//! Turning journal events into ACP session updates.

use tetanus_protocol::types::{KnownEvent, SessionEvent};

use crate::wire::{ContentBlock, SessionUpdate, ToolCallContent, ToolCallStatus};

/// What one journal event becomes on the ACP wire, if anything.
///
/// Three kinds of event cross, and the choice of which is the interesting part.
///
/// Committed assistant messages cross, and raw `assistant/chunk` deltas do not.
/// A chunk is a provider's stream mid-flight: it can be superseded by a retry,
/// and its text arrives again, whole, on the message that closes the step.
/// Forwarding both would show a client the same sentence twice and could leak
/// text from an attempt that was thrown away.
///
/// Tool calls and their results cross, which is where this bridge parts company
/// with upstream's. Upstream keeps tool activity off its automation wire on
/// purpose. ACP has first-class `tool_call` and `tool_call_update` variants,
/// and this workspace's contract already treats the journal as the stream
/// (§7.2), so withholding them would be inventing a second, quieter history
/// for one client to see - exactly what §7.2 rejects.
///
/// Everything else - structural boundaries, reasoning, the session header -
/// produces nothing. ACP has no word for them, and a client cannot act on one.
pub fn updates_of(event: &SessionEvent) -> Vec<SessionUpdate> {
    match event.parse() {
        Some(KnownEvent::AssistantMessage { content, .. }) if !content.is_empty() => {
            vec![SessionUpdate::AgentMessageChunk {
                content: ContentBlock::text(content),
            }]
        }
        Some(KnownEvent::ToolCall {
            id,
            name,
            arguments,
        }) => vec![SessionUpdate::ToolCall {
            tool_call_id: id,
            // The tool's name is the title. Anything friendlier would be this
            // crate writing user-facing copy, which is the presentation lane's
            // to write and not a bridge's to invent.
            title: name,
            // `pending` and not `in_progress`: the call has been asked for, and
            // whether it has started running is not something the journal
            // records separately.
            status: ToolCallStatus::Pending,
            raw_input: Some(arguments),
        }],
        Some(KnownEvent::ToolResult {
            call_id,
            ok,
            content,
            ..
        }) => vec![SessionUpdate::ToolCallUpdate {
            tool_call_id: call_id,
            status: if ok {
                ToolCallStatus::Completed
            } else {
                ToolCallStatus::Failed
            },
            // A failed call's output is still its output. Dropping it would
            // leave a client with a red mark and no reason for it.
            content: vec![ToolCallContent::Content {
                content: ContentBlock::text(content),
            }],
        }],
        _ => Vec::new(),
    }
}
