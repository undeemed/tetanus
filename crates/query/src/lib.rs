//! Reading a session journal as data.
//!
//! The journal is already the whole truth of a session: `session.events` will
//! hand any caller every line of it. What that call will not do is answer a
//! *question* about the lines - which tool ran, which turn a tool failed in,
//! what a range of turns cost - and a surface that wants one today pages the
//! whole log and folds it by hand. This crate is that fold, written once.
//!
//! Three decisions are worth stating before the types.
//!
//! **Position is derived, not carried.** Contract section 4.3.1 puts `turn`
//! and `step` on the structural events (`turn/start`, `step/start`,
//! `assistant/chunk`) and on nothing else: a `tool/call` names no turn. So a
//! reader that wants "every tool call in turn 3" has to know where the turn
//! boundaries are, and [`Journal`] works that out once, in one forward pass,
//! and hangs it on every event as [`Located`]. Deriving it beats adding fields
//! to the journal, which would be a wire change for something already implied
//! by the order of the lines.
//!
//! **An absent clause and an empty one are different questions.** `tools: None`
//! asks about every tool; `tools: Some(vec![])` asks about no tool and matches
//! nothing. Upstream draws the same line, and a `Vec` that could not tell them
//! apart would silently turn a filter built from an empty user selection into
//! "match everything" - the most expensive possible wrong answer.
//!
//! **Nothing here is a closed vocabulary.** The durable event vocabulary grows
//! (`todo/write`, `fs/observed`), so [`Role`] is derived from the type's domain
//! rather than matched against a list, and an unknown type is classified rather
//! than dropped.
//!
//! This crate opens no file and holds no session: it reads through
//! [`EventSource`], which the engine already satisfies, so the same query runs
//! in process and over a carrier.

pub mod aggregate;
pub mod filter;
pub mod journal;
pub mod source;

pub use aggregate::{ToolCallRecord, TurnCost, TurnRow};
pub use filter::{Bound, EventFilter, QueryError, Role};
pub use journal::{Journal, Located, Page, PageResult, Selection};
pub use source::EventSource;
