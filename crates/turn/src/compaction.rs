//! Folding an older span of the conversation into one summary, durably, so a
//! session that outgrows its context window keeps going.
//!
//! A coding session grows until the next request will not fit. Compaction
//! replaces a head-anchored span of the conversation with a single summary
//! message, keeping a verbatim tail of recent work, so the request the
//! provider sees is inside the budget again.
//!
//! **The surface is derived, never stored.** A journal is append-only, so
//! nothing is deleted or rewritten to make a span disappear. What changes is
//! how history is *derived* from it: a `compaction/summary` record names the
//! events it shadows, and the surface event immediately after it takes their
//! place. [`surface`] applies that rule, [`crate::log::derive_messages`] reads
//! the result, and a replayed journal therefore derives the compacted history
//! exactly - the same rule producing the same answer, rather than a second
//! copy that could disagree.
//!
//! **The record and its replacement are adjacent, and that is contractual.**
//! The record states the price and the range; the very next surface event is
//! the replacement. Upstream depends on the same adjacency for the same
//! reason: it lets a bounded fold - one running total and one pending claim -
//! price a replacement without keeping a price per message
//! ([`crate::projections`]).
//!
//! **A cut never splits a tool call from its result.** A request whose
//! assistant message asks for a tool that no later message answers is one a
//! provider refuses. [`tool_pairing_balanced_before`] decides where a cut may
//! land, over the current surface rather than over step markers, because
//! compaction moves surface positions and step markers do not follow.
//!
//! **A summary that is not smaller is not a compaction.** The transaction
//! refuses one, rather than committing a replacement that made the request
//! bigger and would be compacted again on the next step for ever.
//!
//! Parity: upstream `packages/compaction/compaction`, `compaction-basic` and
//! the session-transaction half of `compaction-tool-result-pruner`, whose
//! content transform is already [`crate::prune`]. Upstream's compaction lock
//! (`compaction/start` and `compaction/end` bracketing an awaited summariser)
//! is kept, because the summariser is a provider call and a second compaction
//! entering during it would shadow a range the first one is still holding.
//! Its manual `/compact` command, its per-model policy table and its
//! surface-changed retries are a surface tetanus has not built.

use std::collections::BTreeSet;

use serde_json::Value;

use tetanus_session::{SessionEvent, SessionLog};

use crate::llm::Message;
use crate::log::{derive_messages, topic as log_topic};
use crate::prune::{prune, PruneBudget};
use crate::tokens::{estimate_message, is_surface_event};

/// The durable types this module writes.
///
/// None of them derives to a message: what the model reads is the replacement
/// event, not the record that explains it. Contract section 4.3.2 says the
/// durable vocabulary grows and that a surface passes an unknown type through,
/// which is what makes adding these five safe for a reader that has not
/// learned them.
pub mod topic {
    /// Opens a compaction and holds the lock until `compaction/end`.
    pub const COMPACTION_START: &str = "compaction/start";
    /// The completed summary, the range it shadows, and that range's price.
    /// The next surface event is its replacement.
    pub const COMPACTION_SUMMARY: &str = "compaction/summary";
    /// Closes a compaction, successfully or with the error that stopped it.
    pub const COMPACTION_END: &str = "compaction/end";
    /// The price of one model-free prune replacement. The next surface event
    /// is its replacement.
    pub const COMPACTION_PRUNE: &str = "compaction/prune";
    /// The request envelope a step is about to send: the route, its context
    /// window, and what the system prompt and tool catalog cost.
    ///
    /// It is the anchor `context.breakdown` needs and the window every
    /// compaction decision is taken against. Written before the request rather
    /// than after the answer, so a turn that failed still says what it tried
    /// to send.
    pub const REQUEST_CONTEXT: &str = "request/context";
}

/// The price of a range a replacement is about to shadow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowPrice {
    /// The shadowed events, in surface order.
    pub shadowed_seqs: Vec<u64>,
    /// What that range cost under [`crate::tokens`]'s estimator.
    pub shadowed_token_count: u64,
}

/// The shadow price a record states, or `None` for any other event.
pub fn shadow_price(event: &SessionEvent) -> Option<ShadowPrice> {
    if event.ty != topic::COMPACTION_SUMMARY && event.ty != topic::COMPACTION_PRUNE {
        return None;
    }
    Some(ShadowPrice {
        shadowed_seqs: event
            .data
            .get("shadowed_seqs")
            .and_then(Value::as_array)
            .map(|seqs| seqs.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default(),
        shadowed_token_count: event
            .data
            .get("shadowed_token_count")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

/// The model-visible events of a log, in the order a request carries them,
/// as indices into `events`.
///
/// With no compaction on the log this is every surface event in log order,
/// which is what makes the compacted and uncompacted derivations one function
/// rather than two.
///
/// A replacement takes the *position* of the range it shadows, not the end of
/// the conversation: a summary of the first twenty messages belongs where
/// those twenty were, before the tail that was kept verbatim.
pub fn surface(events: &[SessionEvent]) -> Vec<usize> {
    let mut nodes: Vec<usize> = Vec::new();
    let mut claim: Option<Vec<u64>> = None;

    for (index, event) in events.iter().enumerate() {
        if let Some(price) = shadow_price(event) {
            claim = Some(price.shadowed_seqs);
            continue;
        }
        if !is_surface_event(event) {
            // Any other event expires an armed claim. A record whose next
            // event is not a replacement described a replacement that never
            // landed, and honouring it later would shadow the wrong range.
            claim = None;
            continue;
        }
        match claim.take() {
            Some(shadowed) => {
                let shadowed: BTreeSet<u64> = shadowed.into_iter().collect();
                let at = nodes
                    .iter()
                    .position(|node| shadowed.contains(&events[*node].seq))
                    .unwrap_or(nodes.len());
                nodes.retain(|node| !shadowed.contains(&events[*node].seq));
                nodes.insert(at.min(nodes.len()), index);
            }
            None => nodes.push(index),
        }
    }
    nodes
}

/// How many unanswered tool calls cross the cut before surface position `at`.
///
/// Stated over the surface and over message content, not over step markers:
/// compaction moves surface positions, and a step marker names a position in
/// the log that the surface no longer follows.
fn open_calls_before(events: &[SessionEvent], nodes: &[usize], at: usize) -> i64 {
    let mut open = 0;
    for node in nodes.iter().take(at) {
        let event = &events[*node];
        open += match event.ty.as_str() {
            log_topic::ASSISTANT_MESSAGE => event
                .data
                .get("tool_calls")
                .and_then(Value::as_array)
                .map_or(0, |calls| calls.len() as i64),
            log_topic::TOOL_RESULT => -1,
            _ => 0,
        };
    }
    open
}

/// Whether the cut immediately before surface position `at` splits no
/// tool-call/result pair.
///
/// A cut that splits one produces a request whose assistant message asks for a
/// tool nothing answers, which a provider refuses - so this decides every
/// boundary a compaction may use.
pub fn tool_pairing_balanced_before(events: &[SessionEvent], nodes: &[usize], at: usize) -> bool {
    open_calls_before(events, nodes, at) == 0
}

/// The budgets one compaction decision runs under, in heuristic tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionBudget {
    /// Compact once the request would cost at least this much.
    pub threshold_tokens: u64,
    /// Keep at least this much of the most recent conversation verbatim.
    pub retain_tokens: u64,
}

/// Upstream's own fractions of a model's context window.
pub const DEFAULT_THRESHOLD_RATIO: f64 = 0.8;
/// Upstream's own fraction of the window kept verbatim.
pub const DEFAULT_RETAIN_RATIO: f64 = 0.16;

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("a context window of zero leaves no room for a request")]
    EmptyWindow,
    #[error(
        "retain ({retain}) must be less than threshold ({threshold}): a tail bigger than the \
         budget can never be compacted down to it"
    )]
    RetainExceedsThreshold { retain: u64, threshold: u64 },
    #[error("nothing on this session's surface can be compacted without splitting a tool call")]
    NothingToCompact,
    #[error(
        "the summary is {summary} tokens and the span it replaces is {shadowed}: a replacement \
         that is not smaller would be compacted again for ever"
    )]
    NotSmaller { summary: u64, shadowed: u64 },
    #[error("the summarizer produced nothing to put in the model's place")]
    EmptySummary,
    #[error("summarizing failed: {0}")]
    Summarizer(String),
    #[error("a compaction is already open on this session (started at seq {0})")]
    AlreadyOpen(u64),
    #[error(transparent)]
    Log(#[from] tetanus_session::SessionError),
}

impl CompactionBudget {
    /// Scale the default fractions to one model's context window.
    pub fn for_window(window: u64) -> Result<Self, CompactionError> {
        Self::scaled(window, DEFAULT_THRESHOLD_RATIO, DEFAULT_RETAIN_RATIO)
    }

    /// Scale explicit fractions to one model's context window.
    ///
    /// A retain budget that reaches the threshold is refused where it is set,
    /// not noticed where it is applied: a tail bigger than the whole budget
    /// can never be compacted down to it, so every step would try, fail and
    /// try again.
    pub fn scaled(window: u64, threshold: f64, retain: f64) -> Result<Self, CompactionError> {
        if window == 0 {
            return Err(CompactionError::EmptyWindow);
        }
        let budget = Self {
            threshold_tokens: (window as f64 * threshold) as u64,
            retain_tokens: (window as f64 * retain) as u64,
        };
        if budget.retain_tokens >= budget.threshold_tokens {
            return Err(CompactionError::RetainExceedsThreshold {
                retain: budget.retain_tokens,
                threshold: budget.threshold_tokens,
            });
        }
        Ok(budget)
    }
}

/// The head-anchored span to compact, as surface positions, or `None` when
/// there is nothing to take.
///
/// The tail is walked backwards until `retain_tokens` is covered, and the cut
/// is then moved earlier until it stops splitting a tool-call/result pair. A
/// cut that reaches the head of the surface means the whole conversation is
/// the tail worth keeping, and there is nothing to compact.
pub fn select_range(
    events: &[SessionEvent],
    nodes: &[usize],
    retain_tokens: u64,
) -> Option<(usize, usize)> {
    if nodes.is_empty() {
        return None;
    }
    let mut accumulated = 0;
    let mut keep_from = nodes.len();
    for (position, node) in nodes.iter().enumerate().rev() {
        accumulated += price(&events[*node]);
        keep_from = position;
        if accumulated >= retain_tokens {
            break;
        }
    }
    while keep_from > 0 && !tool_pairing_balanced_before(events, nodes, keep_from) {
        keep_from -= 1;
    }
    // `then`, not `then_some`: the latter evaluates its argument eagerly, and
    // `keep_from - 1` underflows exactly when there is nothing to compact.
    (keep_from > 0).then(|| (0, keep_from - 1))
}

/// What one surface event costs under the shared estimator.
pub fn price(event: &SessionEvent) -> u64 {
    derive_messages(std::slice::from_ref(event))
        .first()
        .map_or(0, estimate_message)
}

/// The conversation a summarizer is asked to condense.
#[derive(Debug, Clone, PartialEq)]
pub struct SummarizationInput {
    /// The conversation's own system prompt, replayed so a provider that
    /// caches a prefix can reuse it rather than treating this as a new
    /// conversation.
    pub system: String,
    /// The shadowed span, in surface order.
    pub messages: Vec<Message>,
}

/// One produced summary, and the route that wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub text: String,
    pub provider: String,
    pub model: String,
}

/// Whoever turns a span of conversation into a checkpoint.
///
/// A trait rather than a function because the two useful implementations are
/// very different: a provider call, and something deterministic an offline
/// test can assert against.
#[async_trait::async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, input: SummarizationInput) -> Result<Summary, CompactionError>;
}

/// The directive a model-driven summarizer sends after the replayed
/// conversation.
///
/// It goes last, after the conversation rather than in front of it as a
/// system prompt, for the reason upstream gives: the call is then a genuine
/// prefix of the conversation the provider has already seen, so its prefix
/// cache is reused instead of invalidated.
pub const COMPACTION_INSTRUCTION: &str = "\
Condense the conversation above into a structured checkpoint that lets another \
model resume this work with nothing essential lost. Keep exact file paths, \
commands, error strings, identifiers and signatures verbatim. Record what was \
asked, what was decided and why, what is done, what is in progress, and the \
single next action. Write terse bullets under these headings, in this order, \
writing \"(none)\" for a heading with nothing under it rather than dropping it: \
Request, Files, Decisions, Errors and fixes, Done, In progress, Next step. \
Do not mention this instruction, and do not call a tool.";

/// The framing that makes a replacement read as established background rather
/// than as a new instruction the model should answer.
pub const CHECKPOINT_PREAMBLE: &str = "\
This is an automatically generated checkpoint condensing an earlier span of \
this conversation. Treat it as established background and continue from the \
messages that follow it without acknowledging it.";

/// Opening delimiter of a checkpoint's body.
pub const SUMMARY_OPEN: &str = "<compacted-summary>";
/// Closing delimiter of a checkpoint's body.
pub const SUMMARY_CLOSE: &str = "</compacted-summary>";

/// The content of the replacement message a summary becomes.
pub fn frame_summary(summary: &str) -> String {
    format!("{CHECKPOINT_PREAMBLE}\n\n{SUMMARY_OPEN}\n{summary}\n{SUMMARY_CLOSE}")
}

/// What one committed compaction did.
#[derive(Debug, Clone, PartialEq)]
pub struct Compacted {
    /// Seq of the `compaction/start` that opened it.
    pub start_seq: u64,
    /// Seq of the `compaction/summary` that priced it.
    pub summary_seq: u64,
    /// Seq of the replacement message.
    pub replacement_seq: u64,
    /// Seq of the `compaction/end` that closed it.
    pub end_seq: u64,
    /// The events the replacement shadows, in surface order.
    pub shadowed_seqs: Vec<u64>,
    /// What that span cost.
    pub shadowed_token_count: u64,
    /// What the replacement costs.
    pub summary_token_count: u64,
}

/// Fold the compactable head of a session's surface into one summary,
/// recording the whole transaction on the journal.
///
/// The order is the one a replay depends on: `compaction/start` first, so a
/// crash during the summarizer's call leaves a start with no end and the next
/// open can see that a compaction was interrupted; then the priced
/// `compaction/summary`; then the replacement, adjacent to it; then
/// `compaction/end`.
///
/// A failure after the start still writes an end, carrying the reason, so the
/// lock is never left held by a compaction that is not running.
pub async fn compact(
    log: &dyn SessionLog,
    summarizer: &dyn Summarizer,
    system: &str,
    budget: CompactionBudget,
) -> Result<Compacted, CompactionError> {
    let events = log.events();
    if let Some(open) = open_compaction(&events) {
        return Err(CompactionError::AlreadyOpen(open));
    }
    let nodes = surface(&events);
    let Some((from, to)) = select_range(&events, &nodes, budget.retain_tokens) else {
        return Err(CompactionError::NothingToCompact);
    };

    let shadowed: Vec<usize> = nodes[from..=to].to_vec();
    let shadowed_seqs: Vec<u64> = shadowed.iter().map(|node| events[*node].seq).collect();
    let shadowed_token_count: u64 = shadowed.iter().map(|node| price(&events[*node])).sum();
    let messages: Vec<Message> = shadowed
        .iter()
        .flat_map(|node| derive_messages(std::slice::from_ref(&events[*node])))
        .collect();

    let start = log.append(
        topic::COMPACTION_START,
        serde_json::json!({
            "shadowed_range": { "start": shadowed_seqs.first(), "end": shadowed_seqs.last() },
        }),
    )?;

    let attempt = summarize_and_commit(
        log,
        summarizer,
        system,
        messages,
        start.seq,
        &shadowed_seqs,
        shadowed_token_count,
    )
    .await;

    match attempt {
        Ok(mut done) => {
            done.end_seq = log
                .append(
                    topic::COMPACTION_END,
                    serde_json::json!({ "start_seq": start.seq }),
                )?
                .seq;
            Ok(done)
        }
        Err(error) => {
            // The close is best effort in the sense that its own failure must
            // not replace the reason the compaction failed: that reason is
            // what a caller can act on.
            let _ = log.append(
                topic::COMPACTION_END,
                serde_json::json!({ "start_seq": start.seq, "error": error.to_string() }),
            );
            Err(error)
        }
    }
}

/// The awaited half, split out so every failure inside it lands on one
/// `compaction/end`.
#[allow(clippy::too_many_arguments)]
async fn summarize_and_commit(
    log: &dyn SessionLog,
    summarizer: &dyn Summarizer,
    system: &str,
    messages: Vec<Message>,
    start_seq: u64,
    shadowed_seqs: &[u64],
    shadowed_token_count: u64,
) -> Result<Compacted, CompactionError> {
    let summary = summarizer
        .summarize(SummarizationInput {
            system: system.to_string(),
            messages,
        })
        .await?;
    if summary.text.trim().is_empty() {
        return Err(CompactionError::EmptySummary);
    }

    let content = frame_summary(&summary.text);
    let summary_token_count = estimate_message(&Message::user(&content));
    if summary_token_count >= shadowed_token_count {
        return Err(CompactionError::NotSmaller {
            summary: summary_token_count,
            shadowed: shadowed_token_count,
        });
    }

    // The record and its replacement are appended with nothing between them.
    // A bounded consumer prices the replacement by the record in front of it,
    // so anything appended between the two would expire the claim and leave
    // the replacement unpriced.
    let record = log.append(
        topic::COMPACTION_SUMMARY,
        serde_json::json!({
            "start_seq": start_seq,
            "summary": summary.text,
            "provider": summary.provider,
            "model": summary.model,
            "shadowed_range": { "start": shadowed_seqs.first(), "end": shadowed_seqs.last() },
            "shadowed_seqs": shadowed_seqs,
            "shadowed_token_count": shadowed_token_count,
        }),
    )?;
    let mut sources = vec![start_seq, record.seq];
    sources.extend_from_slice(shadowed_seqs);
    let replacement = log.append_with_sources(
        log_topic::USER_MESSAGE,
        serde_json::json!({ "content": content, "compacted": true }),
        sources,
    )?;

    Ok(Compacted {
        start_seq,
        summary_seq: record.seq,
        replacement_seq: replacement.seq,
        // Filled by the caller, which owns the close.
        end_seq: replacement.seq,
        shadowed_seqs: shadowed_seqs.to_vec(),
        shadowed_token_count,
        summary_token_count,
    })
}

/// The seq of a `compaction/start` this log never closed, if there is one.
pub fn open_compaction(events: &[SessionEvent]) -> Option<u64> {
    events
        .iter()
        .rev()
        .find(|event| event.ty == topic::COMPACTION_START || event.ty == topic::COMPACTION_END)
        .filter(|event| event.ty == topic::COMPACTION_START)
        .map(|event| event.seq)
}

/// What one pass of the tool-result pruner replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pruned {
    /// The shadowed result and the replacement that took its place.
    pub replacements: Vec<(u64, u64)>,
    /// Characters removed across the pass.
    pub chars_removed: usize,
}

/// Shrink every over-long tool result on the current surface, recording each
/// replacement durably.
///
/// Model-free, so it needs no provider and costs nothing: this is the cheap
/// half of compaction, and the half worth trying before a summary. Each
/// replacement is a `compaction/prune` record stating the shadowed result's
/// price, immediately followed by the shortened result - the same adjacency
/// [`compact`] keeps, and for the same consumer.
pub fn prune_results(log: &dyn SessionLog, budget: PruneBudget) -> Result<Pruned, CompactionError> {
    let events = log.events();
    let nodes = surface(&events);
    let mut done = Pruned {
        replacements: Vec::new(),
        chars_removed: 0,
    };

    for node in nodes {
        let event = &events[node];
        if event.ty != log_topic::TOOL_RESULT {
            continue;
        }
        let content = event
            .data
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(shortened) = prune(content, budget) else {
            continue;
        };
        let before = content.chars().count();
        let after = shortened.chars().count();

        log.append(
            topic::COMPACTION_PRUNE,
            serde_json::json!({
                "shadowed_range": { "start": event.seq, "end": event.seq },
                "shadowed_seqs": [event.seq],
                "shadowed_token_count": price(event),
            }),
        )?;
        // Everything the shadowed result carried except its content, so the
        // replacement answers the same call and derives to the same shape.
        let mut data = event.data.clone();
        data["content"] = Value::String(shortened);
        data["pruned"] = Value::Bool(true);
        let replacement = log.append_with_sources(log_topic::TOOL_RESULT, data, vec![event.seq])?;

        done.replacements.push((event.seq, replacement.seq));
        done.chars_removed += before.saturating_sub(after);
    }
    Ok(done)
}

/// A summarizer that writes the checkpoint itself, with no model.
///
/// It exists so the whole transaction - the records, the adjacency, the
/// replacement, the derived history and the replay - can be asserted offline
/// and deterministically. It is a real summarizer, not a stub: it keeps the
/// first line of every message it was given, which is a genuine if crude
/// checkpoint, and it is what a deployment with no second route can fall back
/// on rather than failing a turn it could have continued.
#[derive(Debug, Default)]
pub struct OutlineSummarizer;

/// The provider name an outline summary records, so a reader of the journal
/// can tell a model's checkpoint from this one.
pub const OUTLINE_PROVIDER: &str = "outline";

#[async_trait::async_trait]
impl Summarizer for OutlineSummarizer {
    async fn summarize(&self, input: SummarizationInput) -> Result<Summary, CompactionError> {
        let mut lines = Vec::new();
        for message in &input.messages {
            let first = message
                .content
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("");
            let head: String = first.chars().take(120).collect();
            let calls: Vec<&str> = message
                .tool_calls
                .iter()
                .map(|call| call.name.as_str())
                .collect();
            match (head.is_empty(), calls.is_empty()) {
                (true, true) => continue,
                (true, false) => {
                    lines.push(format!("- {}: called {}", role(message), calls.join(", ")))
                }
                (false, true) => lines.push(format!("- {}: {head}", role(message))),
                (false, false) => lines.push(format!(
                    "- {}: {head} (called {})",
                    role(message),
                    calls.join(", ")
                )),
            }
        }
        Ok(Summary {
            text: lines.join("\n"),
            provider: OUTLINE_PROVIDER.to_string(),
            model: OUTLINE_PROVIDER.to_string(),
        })
    }
}

fn role(message: &Message) -> &'static str {
    message.role.as_str()
}

/// A summarizer that asks a model, through the same adapter seam a turn uses.
///
/// The call replays the conversation's own system prompt and the shadowed span
/// verbatim, then appends [`COMPACTION_INSTRUCTION`] as the last message. That
/// order is the point: the request is then a genuine prefix of the one the
/// provider already answered, so a provider that caches prefixes reuses it.
///
/// The catalog is deliberately not offered. A summarizer that could call a
/// tool would take an action inside a compaction, which is a side effect
/// nobody asked for while the session's history is being rewritten.
pub struct LlmSummarizer {
    adapter: std::sync::Arc<dyn crate::llm::LlmAdapter>,
    model: String,
    max_tokens: Option<u32>,
}

impl LlmSummarizer {
    pub fn new(
        adapter: std::sync::Arc<dyn crate::llm::LlmAdapter>,
        model: impl Into<String>,
        max_tokens: Option<u32>,
    ) -> Self {
        Self {
            adapter,
            model: model.into(),
            max_tokens,
        }
    }
}

#[async_trait::async_trait]
impl Summarizer for LlmSummarizer {
    async fn summarize(&self, input: SummarizationInput) -> Result<Summary, CompactionError> {
        let mut messages = Vec::with_capacity(input.messages.len() + 2);
        if !input.system.is_empty() {
            messages.push(Message::system(&input.system));
        }
        messages.extend(input.messages);
        messages.push(Message::user(COMPACTION_INSTRUCTION));

        let request = crate::llm::ModelRequest {
            provider: self.adapter.provider().to_string(),
            model: self.model.clone(),
            messages,
            tools: Vec::new(),
            max_tokens: self.max_tokens,
        };
        let mut sink = DiscardChunks;
        let response = self
            .adapter
            .stream(&request, &mut sink)
            .await
            .map_err(|error| CompactionError::Summarizer(error.to_string()))?;
        // A checkpoint the provider cut off at its output cap is an incomplete
        // summary, and committing one would shadow a span of real conversation
        // behind a half-written replacement.
        if response.truncated() {
            return Err(CompactionError::Summarizer(
                "the summary hit the provider's output cap, so it is incomplete".into(),
            ));
        }
        Ok(Summary {
            text: response.content,
            provider: request.provider,
            model: request.model,
        })
    }
}

/// The summarizer's stream is not the session's: its deltas are not durable
/// facts of this conversation, and logging them would put the checkpoint's own
/// drafting into the history it is condensing.
struct DiscardChunks;

#[async_trait::async_trait]
impl crate::llm::ChunkSink for DiscardChunks {
    async fn chunk(&mut self, _chunk: crate::llm::StreamChunk) -> Result<(), crate::llm::LlmError> {
        Ok(())
    }
}
