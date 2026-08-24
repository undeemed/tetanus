//! What a request costs, priced before a provider says.
//!
//! One fixed-density heuristic prices every model-visible thing: four
//! characters to a token, four tokens of framing per content block, four more
//! for the role a message carries. It is deliberately crude. Its job is to give
//! a number when no provider has answered yet - for a context gauge, for a
//! compaction decision, for a budget - and to give the same number to every
//! reader, which an exact tokenizer per provider would not.
//!
//! The surface is the part of the log the model sees: `user/message`,
//! `assistant/message` and `tool/result`, in log order. [`TokenSurface`] folds
//! those events into one priced node each, so a caller can ask what the
//! conversation carries without re-deriving history itself.
//!
//! Parity: upstream `packages/llm/token-meter/src/estimate.ts` for the
//! heuristic and `surface-fold.ts` for the fold. Two upstream parts are not
//! here. A measurement that anchors on real provider usage needs the request
//! envelope on the log, and tetanus logs no `request/header` event. Replacing a
//! range of surface nodes is compaction, which tetanus does not do. Both are
//! rows in `docs/parity.md`.

use tetanus_session::SessionEvent;

use crate::llm::{Message, ModelRequest, Role};
use crate::log::derive_messages;
use crate::tools::ToolSchema;

/// Characters to a token under the fixed-density heuristic.
pub const CHARS_PER_TOKEN: usize = 4;
/// Per-block framing: the JSON structure and the type tag around content.
pub const BLOCK_OVERHEAD: u64 = 4;
/// Per-message framing: the role field every priced message carries.
pub const ROLE_OVERHEAD: u64 = 4;

/// One priced node of the current surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceNode {
    /// The durable event this node prices.
    pub seq: u64,
    /// Its heuristic tokens; zero when the event derives no message.
    pub tokens: u64,
}

/// The priced surface, folded from the log in order.
///
/// A compaction replaces a range of nodes with one summary, and
/// [`TokenSurface::of`] reads the log through
/// [`crate::compaction::surface`], so the priced surface is the surface the
/// model actually sees. [`TokenSurface::fold`] is the append-only half, for a
/// caller placing one event at a time on a surface it is building itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenSurface {
    nodes: Vec<SurfaceNode>,
    total: u64,
}

impl TokenSurface {
    /// The priced surface of a whole log, in the order a request carries it.
    pub fn of(events: &[SessionEvent]) -> Self {
        let mut surface = Self::default();
        for index in crate::compaction::surface(events) {
            surface.fold(&events[index]);
        }
        surface
    }

    /// Place one durable event on the surface.
    ///
    /// An event that derives no message - a step marker, a raw chunk, an
    /// assistant message that said nothing - is not on the surface and adds no
    /// node. An `assistant/message` that derives nothing is the one exception
    /// upstream keeps: it is a surface event, so it takes its place at zero.
    pub fn fold(&mut self, event: &SessionEvent) {
        if !is_surface_event(event) {
            return;
        }
        let tokens = derive_messages(std::slice::from_ref(event))
            .first()
            .map_or(0, estimate_message);
        self.nodes.push(SurfaceNode {
            seq: event.seq,
            tokens,
        });
        self.total += tokens;
    }

    /// The nodes, head to tail in model-visible order.
    pub fn nodes(&self) -> &[SurfaceNode] {
        &self.nodes
    }

    /// Total heuristic tokens across the surface.
    pub fn total_tokens(&self) -> u64 {
        self.total
    }
}

/// Whether an event is one the model sees. The three surface types are the ones
/// [`derive_messages`] projects, and the ones that cite their sources.
pub fn is_surface_event(event: &SessionEvent) -> bool {
    use crate::log::topic;
    matches!(
        event.ty.as_str(),
        topic::USER_MESSAGE | topic::ASSISTANT_MESSAGE | topic::TOOL_RESULT
    )
}

/// Price one model-visible message: its content blocks plus role framing.
///
/// tetanus carries a message's text as one string rather than a list of blocks,
/// so a message has at most one text block, one block per tool call, and - on a
/// tool result - one block wrapping the text block it reports. Empty text is no
/// block at all, not a block worth its framing.
pub fn estimate_message(message: &Message) -> u64 {
    let mut tokens = ROLE_OVERHEAD;
    if !message.content.is_empty() {
        let text = estimate_text(&message.content);
        tokens += match message.role {
            // A tool result is a block that contains the reported content.
            Role::Tool => text + BLOCK_OVERHEAD,
            _ => text,
        };
    }
    for call in &message.tool_calls {
        tokens += chars(call.name.len()) + chars(call.arguments.to_string().len()) + BLOCK_OVERHEAD;
    }
    tokens
}

/// Price the tool catalog the request advertises. It is one JSON structure, so
/// it is priced once rather than per tool. An empty catalog costs nothing.
pub fn estimate_tools(tools: &[ToolSchema]) -> u64 {
    if tools.is_empty() {
        return 0;
    }
    let json = serde_json::to_string(tools).unwrap_or_default();
    chars(json.len()) + BLOCK_OVERHEAD
}

/// Price a whole assembled request: its tool catalog plus every message,
/// system prompt included, because tetanus carries that as a message too.
pub fn estimate_request(request: &ModelRequest) -> u64 {
    estimate_tools(&request.tools) + request.messages.iter().map(estimate_message).sum::<u64>()
}

/// One free-standing piece of text - a system prompt, say - as a message
/// carrying nothing else. Published because the request envelope is priced
/// outside a `Message`, and pricing it any other way would give a figure that
/// did not add up with the rest.
pub fn estimate_text_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    ROLE_OVERHEAD + estimate_text(text)
}

/// One text block: its content at the fixed density, plus block framing.
fn estimate_text(text: &str) -> u64 {
    chars(text.len()) + BLOCK_OVERHEAD
}

/// Characters at the fixed density, rounded up: no content is free, but no
/// partial token is dropped either.
fn chars(len: usize) -> u64 {
    len.div_ceil(CHARS_PER_TOKEN) as u64
}
