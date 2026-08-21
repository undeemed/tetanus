//! Phase ① turn engine: one full documented dsh turn, on a typed registry, a
//! four-mode event bus, and a durable session log.
//!
//! - [`engine`] drives the turn flow and owns the documented event order.
//! - [`events`] declares the live extension points a turn dispatches.
//! - [`log`] declares the durable session-event vocabulary and derives model
//!   history from it.
//! - [`prompt`] is the named section registry the assembly starts from.
//! - [`repair`] writes the closers an interrupted journal is missing.
//! - [`llm`] is the model-provider seam, with a deterministic offline adapter
//!   and the DeepSeek chat-completions adapter.
//! - [`schema`] checks a call's arguments against the schema its tool published.
//! - [`tools`] is the model-facing capability registry.
//! - [`boot`] composes them through the typed service registry.
//! - [`trace`] reads back the ordered event sequence of a run.

pub mod boot;
pub mod engine;
pub mod events;
pub mod interrupt;
pub mod llm;
pub mod log;
pub mod prompt;
pub mod repair;
pub mod schema;
pub mod tokens;
pub mod tools;
pub mod trace;

pub use engine::{TurnConfig, TurnEngine, TurnError, TurnOutcome};
pub use events::{StopReason, FAILED_STOP_REASON};
pub use trace::{TraceEntry, TurnTrace};
