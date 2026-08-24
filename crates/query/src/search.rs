//! Searching a session's words, and saying which of them the model can still
//! see.
//!
//! [`crate::EventFilter`]'s `text` clause is a substring test: it answers "does
//! this event contain this run of characters". That is the right tool for a
//! filter and the wrong one for a search box, where a person types two words in
//! the order they thought of them and expects a hit on an event containing
//! both. This module is the second thing.
//!
//! Three decisions carry it, and the first is the one that matters.
//!
//! **A hit is labelled by its compaction surface, and this crate never works
//! that surface out.** When a conversation outgrows its window, tetanus does
//! not delete anything - the journal is append-only and `compaction::surface`
//! changes how history is *derived*. So a session can hold text that is on the
//! log and is no longer model-visible, and a search that returned it unlabelled
//! would show a person a sentence the model has since lost, with nothing saying
//! so. Upstream carries the same field for the same reason.
//!
//! What this crate will not do is derive it. `AGENTS.md` is explicit that
//! anything reading model history goes through the engine's own fold, because a
//! second reader disagrees with the first the day a session compacts. The
//! obvious alternative - depend on `tetanus-turn` and call `compaction::surface`
//! here - drags an HTTP client into a crate whose whole virtue is that it opens
//! nothing. So the surface is an **input**: a caller that knows it supplies it
//! through [`crate::Journal::with_surface`], and a caller that does not gets
//! [`Surface::Unknown`] rather than a cheerful `Current` nobody checked.
//!
//! **Order is by seq, and there is no ranking.** Upstream ranks because its
//! provider is SQLite's full-text index, which computes a relevance score. A
//! scan has no such number, and inventing one here - term counts, field
//! weights - would be this crate making up a relevance model no caller asked
//! for and none could tune. Sequence order is a fact about the session.
//!
//! **A cursor belongs to the query that produced it.** Paging a search is
//! resumable, and the failure worth designing against is a caller that pages a
//! *different* query with a cursor from an earlier one: the seqs line up, the
//! answer looks plausible, and it is wrong. Every cursor carries a fingerprint
//! of the query it came from and is refused against any other.

use std::collections::BTreeSet;

use crate::filter::{EventFilter, QueryError};
use crate::journal::Located;

/// Whether the model can still see an event, at the time the journal was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Surface {
    /// On the log and model-visible.
    Current,
    /// On the log, and replaced by a compaction summary. The words are real and
    /// the model no longer has them.
    Shadowed,
    /// Nobody supplied a surface, so this crate is not going to guess. Distinct
    /// from `Current` on purpose: "we checked and it is visible" and "we did not
    /// check" are different facts, and a caller that renders them the same is
    /// choosing to.
    Unknown,
}

/// What to search for.
///
/// Terms are matched case-insensitively against the event's own words - the
/// same text the `text` filter clause reads, which is conversation only: a user
/// message, a committed assistant message, a tool call's name and arguments, a
/// tool result. A stream chunk is left out because its text arrives again on
/// the message that closes the step, and reasoning is the model's scratch paper.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchQuery {
    /// The words to look for. Empty is refused rather than answered with
    /// everything: a search box that submits blank means "I have not typed
    /// anything yet", and the whole session is the least useful possible reply.
    pub terms: Vec<String>,
    /// Require every term (`true`) or any one of them (`false`).
    #[serde(default)]
    pub all: bool,
    /// Narrow the corpus before searching it. The same filter the rest of this
    /// crate takes, so "every failed shell call mentioning timeout" is one
    /// query rather than two passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<EventFilter>,
    /// How many hits to return. Clamped to [`MAX_HITS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// The most hits one page will carry, whatever a caller asks for.
///
/// Present for the reason the contract's own page cap is: a caller that asks
/// for everything from a long session should get a page and a cursor, not a
/// frame nothing can hold.
pub const MAX_HITS: u32 = 200;

/// How much of the event's text comes back with a hit.
const SNIPPET_WIDTH: usize = 160;

impl SearchQuery {
    pub fn new(terms: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            terms: terms.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    /// Require every term rather than any.
    pub fn all(mut self) -> Self {
        self.all = true;
        self
    }

    pub fn filter(mut self, filter: EventFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Refuse a query that cannot be answered, before it is answered with an
    /// empty page that reads like a fact about the session.
    pub fn validate(&self) -> Result<(), QueryError> {
        let usable: Vec<&String> = self
            .terms
            .iter()
            .filter(|term| !term.trim().is_empty())
            .collect();
        if usable.is_empty() {
            // `InvalidFilter`, not a new variant: the ask itself is
            // malformed, which is exactly what that case already means, and
            // widening a public error enum during a landing queue buys nothing
            // a sentence does not.
            return Err(QueryError::InvalidFilter(
                "a search needs at least one term with a character in it".to_string(),
            ));
        }
        if let Some(filter) = &self.filter {
            filter.validate()?;
        }
        Ok(())
    }

    /// The terms, lowercased, with the blank ones dropped.
    fn usable(&self) -> Vec<String> {
        self.terms
            .iter()
            .map(|term| term.trim().to_lowercase())
            .filter(|term| !term.is_empty())
            .collect()
    }

    /// A stable fingerprint of what this query asks, so a cursor cannot be
    /// carried to a different one.
    ///
    /// Deliberately covers the corpus-shaping fields and not `limit`: changing
    /// the page size mid-page is a caller's prerogative and does not change
    /// which events match.
    fn fingerprint(&self) -> u64 {
        let mut terms = self.usable();
        terms.sort();
        let shape = serde_json::json!({
            "terms": terms,
            "all": self.all,
            "filter": self.filter,
        });
        fnv1a(shape.to_string().as_bytes())
    }
}

/// One matching event.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Hit {
    pub seq: u64,
    pub time: u64,
    /// The event type, so a caller can tell a tool result from what a person
    /// typed without a second lookup.
    pub ty: String,
    pub turn: Option<u64>,
    pub step: Option<u32>,
    pub tool: Option<String>,
    pub surface: Surface,
    /// The words around the first match, or the whole text where it is short.
    pub snippet: String,
    /// Which of the query's terms this event actually contained, lowercased and
    /// in query order. A caller showing why something matched reads this rather
    /// than re-deriving it.
    pub matched: Vec<String>,
}

/// One page of hits.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchPage {
    pub hits: Vec<Hit>,
    /// How many events matched in the whole session, not just on this page.
    ///
    /// Counted rather than estimated, because the corpus is already in memory
    /// and a caller drawing "1-20 of 3" needs the 3 to be true.
    pub total: usize,
    /// Where to resume, or `None` when this page reached the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
}

/// An opaque continuation, bound to the query that produced it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cursor {
    /// Resume strictly after this seq.
    after_seq: u64,
    /// The query this cursor came from.
    query: u64,
}

impl Cursor {
    /// Render as one opaque token, for a caller that has to put it on a wire or
    /// in a URL.
    pub fn encode(&self) -> String {
        format!("{:x}.{:x}", self.query, self.after_seq)
    }

    /// Read one back.
    pub fn decode(token: &str) -> Result<Self, QueryError> {
        // `InvalidWindow`: a cursor is paging, and that is the case this
        // crate already uses for a window it cannot serve.
        let bad = || QueryError::InvalidWindow("not a cursor this crate issued".to_string());
        let (query, after) = token.split_once('.').ok_or_else(bad)?;
        Ok(Self {
            query: u64::from_str_radix(query, 16).map_err(|_| bad())?,
            after_seq: u64::from_str_radix(after, 16).map_err(|_| bad())?,
        })
    }
}

/// Run one search over positioned events.
///
/// `current` is the set of seqs the model can still see, or `None` when nobody
/// supplied one - see [`Surface::Unknown`].
pub(crate) fn search(
    events: &[Located],
    current: Option<&BTreeSet<u64>>,
    query: &SearchQuery,
    from: Option<&Cursor>,
) -> Result<SearchPage, QueryError> {
    query.validate()?;
    let fingerprint = query.fingerprint();

    if let Some(cursor) = from {
        if cursor.query != fingerprint {
            return Err(QueryError::InvalidWindow(
                "this cursor came from a different search; page the query that issued it, or \
                 start again"
                    .to_string(),
            ));
        }
    }

    let terms = query.usable();
    let limit = query.limit.unwrap_or(MAX_HITS).clamp(1, MAX_HITS) as usize;

    let selected: Vec<&Located> = match &query.filter {
        Some(filter) => events.iter().filter(|e| e.matches(filter)).collect(),
        None => events.iter().collect(),
    };

    let mut total = 0;
    let mut consumed = 0;
    let mut hits = Vec::new();
    for event in selected {
        let (Some(text), Some(lowered)) = (event.text.as_deref(), event.lowered()) else {
            continue;
        };
        let matched: Vec<String> = terms
            .iter()
            .filter(|term| lowered.contains(term.as_str()))
            .cloned()
            .collect();
        let hit = if query.all {
            matched.len() == terms.len()
        } else {
            !matched.is_empty()
        };
        if !hit {
            continue;
        }

        // Counted before the cursor is applied: `total` is a fact about the
        // session, and a caller drawing "showing 20 of 300" would otherwise see
        // the total shrink as it paged.
        total += 1;
        if from.is_some_and(|cursor| event.seq() <= cursor.after_seq) {
            // A *match* already delivered on an earlier page, which is the only
            // thing that may be subtracted from the total when deciding whether
            // more remain. Counting events here instead would be wrong the
            // moment a non-matching event sits before the cursor - which is
            // every real session.
            consumed += 1;
            continue;
        }
        if hits.len() < limit {
            hits.push(Hit {
                seq: event.seq(),
                time: event.time(),
                ty: event.ty().to_string(),
                turn: event.turn,
                step: event.step,
                tool: event.tool.clone(),
                surface: surface_of(event.seq(), current),
                snippet: snippet(text, lowered, &matched),
                matched,
            });
        }
    }

    let last = hits.last().map(|hit| hit.seq);
    let more = total > consumed + hits.len();
    Ok(SearchPage {
        hits,
        total,
        cursor: match (more, last) {
            (true, Some(after_seq)) => Some(Cursor {
                after_seq,
                query: fingerprint,
            }),
            _ => None,
        },
    })
}

fn surface_of(seq: u64, current: Option<&BTreeSet<u64>>) -> Surface {
    match current {
        None => Surface::Unknown,
        Some(current) if current.contains(&seq) => Surface::Current,
        Some(_) => Surface::Shadowed,
    }
}

/// The words around the first match.
///
/// Character-based rather than byte-based: slicing a UTF-8 string at a byte
/// offset panics mid-character, and a transcript is exactly where a multi-byte
/// character turns up.
fn snippet(text: &str, lowered: &str, matched: &[String]) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= SNIPPET_WIDTH {
        return text.to_string();
    }

    // Where the first matching term starts, in characters.
    let at = matched
        .iter()
        .filter_map(|term| lowered.find(term.as_str()))
        .min()
        .map(|byte_at| lowered[..byte_at].chars().count())
        .unwrap_or(0);

    let start = at.saturating_sub(SNIPPET_WIDTH / 4);
    let end = (start + SNIPPET_WIDTH).min(chars.len());
    let start = end.saturating_sub(SNIPPET_WIDTH);

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// FNV-1a, because a cursor's fingerprint needs to be stable across processes
/// and `DefaultHasher` explicitly is not.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}
