//! Phase ① turn engine: one full documented dsh turn, on a typed registry, a
//! four-mode event bus, and a durable session log.
//!
//! - [`engine`] drives the turn flow and owns the documented event order.
//! - [`approval`] decides whether one tool call may run, and audits it.
//! - [`events`] declares the live extension points a turn dispatches.
//! - [`fs`] decides whether a path the model chose is inside its workspace.
//! - [`log`] declares the durable session-event vocabulary and derives model
//!   history from it.
//! - [`process`] runs one external command, bounded in output and in time.
//! - [`prompt`] is the named section registry the assembly starts from.
//! - [`prune`] shrinks a tool result that is too long to keep whole.
//! - [`questions`] asks the user something a tool cannot decide alone.
//! - [`repair`] writes the closers an interrupted journal is missing.
//! - [`instructions`] reads the conventions a project keeps in its repository.
//! - [`llm`] is the model-provider seam, with a deterministic offline adapter
//!   and the DeepSeek chat-completions adapter.
//! - [`schema`] checks a call's arguments against the schema its tool published.
//! - [`tools`] is the model-facing capability registry.
//! - [`boot`] composes them through the typed service registry.
//! - [`trace`] reads back the ordered event sequence of a run.

pub mod approval;
pub mod boot;
pub mod engine;
pub mod events;
pub mod fs;
pub mod instructions;
pub mod interrupt;
pub mod llm;
pub mod log;
pub mod process;
pub mod prompt;
pub mod prune;
pub mod questions;
pub mod repair;
pub mod schema;
pub mod tokens;
pub mod tools;
pub mod trace;

pub use engine::{TurnConfig, TurnEngine, TurnError, TurnOutcome};
pub use events::{StopReason, FAILED_STOP_REASON};
pub use trace::{TraceEntry, TurnTrace};
