//! Phase ① turn engine: one full documented dsh turn, on a typed registry, a
//! four-mode event bus, and a durable session log.
//!
//! - [`engine`] drives the turn flow and owns the documented event order.
//! - [`approval`] decides whether one tool call may run, and audits it.
//! - [`compaction`] folds an older span of the conversation into one summary
//!   when the next request would not fit, durably enough to replay.
//! - [`projections`] are the priced folds over a journal: what was charged,
//!   how full the context is, and what it is made of.
//! - [`events`] declares the live extension points a turn dispatches.
//! - [`fs`] decides whether a path the model chose is inside its workspace.
//! - [`inbox`] queues input that arrived while the loop was busy, durably,
//!   until a boundary can carry it.
//! - [`log`] declares the durable session-event vocabulary and derives model
//!   history from it.
//! - [`prompt`] is the named section registry the assembly starts from.
//!
//! Running an external command is `tetanus-exec`, which sits above this crate:
//! it is a consumer of [`tools`] rather than a part of the loop, and keeping
//! it out of here is what lets a subprocess seam depend on the tool vocabulary
//! instead of the other way round.
//! - [`prune`] shrinks a tool result that is too long to keep whole.
//! - [`questions`] asks the user something a tool cannot decide alone.
//! - [`repair`] writes the closers an interrupted journal is missing.
//! - [`instructions`] reads the conventions a project keeps in its repository.
//! - [`lsp`] runs a language server over stdio and answers the precise
//!   navigation questions textual search cannot.
//! - [`llm`] is the model-provider seam, with a deterministic offline adapter
//!   and the DeepSeek chat-completions adapter.
//! - [`schema`] checks a call's arguments against the schema its tool published.
//! - [`tools`] is the model-facing capability registry.
//! - [`boot`] composes them through the typed service registry.
//! - [`trace`] reads back the ordered event sequence of a run.
//! - [`workflow`] runs multi-step work that outlives the turn that asked for
//!   it, recording every boundary so a restart can continue it.

pub mod approval;
pub mod boot;
pub mod compaction;
pub mod context;
pub mod engine;
pub mod events;
pub mod fs;
pub mod inbox;
pub mod instructions;
pub mod interrupt;
pub mod llm;
pub mod log;
pub mod lsp;
pub mod projections;
pub mod prompt;
pub mod prune;
pub mod questions;
pub mod repair;
pub mod schema;
pub mod tokens;
pub mod tools;
pub mod trace;
pub mod workflow;

pub use engine::{TurnConfig, TurnEngine, TurnError, TurnOutcome};
pub use events::{StopReason, FAILED_STOP_REASON};
pub use trace::{TraceEntry, TurnTrace};
