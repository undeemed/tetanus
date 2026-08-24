//! Durable session-event vocabulary and the projection of model history from
//! it. Model-visible means logged: anything that reaches a model request is
//! reconstructable from these events.

use tetanus_session::SessionEvent;

use crate::llm::{Message, Role};
use crate::tools::ToolCall;

/// The durable event types Phase ① writes, in the order one full step emits
/// them. `turn/*`, `step/*`, `user/message`, `assistant/*` and `tool/*` are all
/// durable session events.
pub mod topic {
    pub const TURN_START: &str = "turn/start";
    pub const STEP_START: &str = "step/start";
    pub const USER_MESSAGE: &str = "user/message";
    pub const ASSISTANT_CHUNK: &str = "assistant/chunk";
    pub const ASSISTANT_MESSAGE: &str = "assistant/message";
    pub const TOOL_CALL: &str = "tool/call";
    pub const TOOL_RESULT: &str = "tool/result";
    pub const STEP_END: &str = "step/end";
    pub const TURN_END: &str = "turn/end";

    /// The decision audit of contract section 4.4.7. `approval/asked` and
    /// `approval/decided` are one pair per question, sharing an `id`;
    /// `approval/policy` is a switch, and the last one is the session's.
    ///
    /// None of the three derives to a message: what the model learns about a
    /// denial is the `tool/result` it gets, not the audit of how that was
    /// decided.
    pub const APPROVAL_ASKED: &str = "approval/asked";
    pub const APPROVAL_DECIDED: &str = "approval/decided";
    pub const APPROVAL_POLICY: &str = "approval/policy";

    /// The user-question audit of contract section 4.4.3, and the same pair
    /// rule: one `question/asked` and exactly one `question/answered` sharing
    /// an `id`, inside the turn that needed the answer.
    ///
    /// Neither derives to a message. What the model learns is the `tool/result`
    /// the asking tool produced, and a transcript that showed it the audit
    /// would be feeding it the harness's own bookkeeping as conversation.
    pub const QUESTION_ASKED: &str = "question/asked";
    pub const QUESTION_ANSWERED: &str = "question/answered";

    /// What a turn told the model about the world outside the conversation,
    /// gathered once per turn (contract section 4.4.8, [`crate::context`]).
    ///
    /// It does derive to a message, and it is the only durable type whose
    /// derivation depends on where it sits: the *last* one on the journal
    /// becomes a `user` message and every earlier one is skipped. A long
    /// session accumulates them, and yesterday's date is worse than no date.
    pub const CONTEXT_SNAPSHOT: &str = "context/snapshot";

    /// One mutation of the queue of input waiting for a boundary the loop has
    /// not reached yet ([`crate::inbox`]). The queues are a replay-once fold
    /// of these, so the record carries normalized coordinates rather than what
    /// a caller asked for.
    ///
    /// It does not derive to a message. What the model sees is the
    /// `user/message` written when a claimed prompt enters a turn; deriving
    /// the queue as well would show it every message twice, once while it was
    /// still waiting.
    pub const INBOX_SPLICED: &str = "agent/inbox/spliced";
}

/// Derive the model history from the log. Replay is re-derivation from the
/// same events, never a second stored copy.
///
/// Raw `assistant/chunk` events are preserved on the log for replay and UI
/// fidelity but are not part of history - the `assistant/message` that cites
/// them is. An `assistant/message` with empty content and no tool calls stays
/// out of derived history while its durable event keeps usage and sources.
///
/// A compaction on the log is honoured here rather than anywhere else, which
/// is what makes a replayed journal derive the compacted history: the events a
/// `compaction/summary` or `compaction/prune` shadows are not part of the
/// derivation, and the replacement stands in their place
/// ([`crate::compaction::surface`]). A log with no compaction on it derives
/// exactly as it always did, because its surface is every surface event in log
/// order.
pub fn derive_messages(events: &[SessionEvent]) -> Vec<Message> {
    let mut out = Vec::new();
    let mut context = newest_context(events);
    for index in crate::compaction::surface(events) {
        // The runtime context enters the history where it sits on the journal:
        // after everything the turn inherited, immediately before the message
        // that opened this turn. Section 4.4.8 puts it after the retained
        // history for a caching reason - a sentence that changes every turn
        // must not sit in the prompt prefix a provider caches - and the
        // journal's own order already satisfies that.
        //
        // Not at the very end, which is the other reading of the same clause:
        // a request whose last message is a machine-written status block ends
        // in something no one asked for, and both a model and this crate's own
        // mock read the last message as the thing to answer.
        if let Some((at, text)) = context.take() {
            if index > at {
                out.push(Message::user(text));
            } else {
                context = Some((at, text));
            }
        }
        let event = &events[index];
        match event.ty.as_str() {
            topic::USER_MESSAGE => {
                out.push(Message::user(string_field(event, "content")));
            }
            topic::ASSISTANT_MESSAGE => {
                let content = string_field(event, "content");
                let tool_calls: Vec<ToolCall> = event
                    .data
                    .get("tool_calls")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                if content.is_empty() && tool_calls.is_empty() {
                    continue;
                }
                out.push(Message::assistant(content, tool_calls));
            }
            topic::TOOL_RESULT => {
                out.push(Message::tool(
                    string_field(event, "call_id"),
                    string_field(event, "content"),
                ));
            }
            _ => {}
        }
    }
    // A snapshot with nothing after it - the ordinary case, since it is written
    // at the top of a turn whose first message has not been logged yet.
    if let Some((_, text)) = context {
        out.push(Message::user(text));
    }
    out
}

/// The message the last `context/snapshot` derives to, and where that snapshot
/// sits on the journal.
///
/// Only the newest one travels. A turn writes one, so a long session
/// accumulates them, and yesterday's date is worse than no date. The earlier
/// ones stay on the journal, because the journal records what happened and a
/// reader may want to know what the model was told at the time.
///
/// Rendering here rather than storing the rendered text is the choice section
/// 4.4.8 fixes: the parts are the record, the joining rule reproduces the text,
/// and a surface that wants to show which provider said what still can.
fn newest_context(events: &[SessionEvent]) -> Option<(usize, String)> {
    let (at, snapshot) = events
        .iter()
        .enumerate()
        .rev()
        .find(|(_, event)| event.ty == topic::CONTEXT_SNAPSHOT)?;
    let parts: Vec<crate::context::ContextPart> = snapshot
        .data
        .get("parts")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    let text = crate::context::render(&parts);
    (!text.is_empty()).then_some((at, text))
}

/// Prepend the system prompt to derived history for the wire request.
pub fn with_system(system: &str, history: Vec<Message>) -> Vec<Message> {
    let mut messages = Vec::with_capacity(history.len() + 1);
    if !system.is_empty() {
        messages.push(Message::system(system));
    }
    messages.extend(history);
    debug_assert!(messages
        .iter()
        .all(|m| m.role != Role::System || !m.content.is_empty()));
    messages
}

fn string_field(event: &SessionEvent, key: &str) -> String {
    event
        .data
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}
