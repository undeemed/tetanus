//! The built-in feature tools: the ones that make the harness usable rather
//! than merely capable.
//!
//! Each is a registered tool plus the state it keeps, and the state is kept in
//! exactly one place - the append-only session journal. Nothing here holds a
//! cache a replay could disagree with: a fold over the log *is* the state, so a
//! resumed session sees what the session that wrote it saw, and a reader of the
//! journal can reconstruct it without running anything.
//!
//! That rule is why every module has the same shape. A durable event type, a
//! fold over the log, a tool that appends and answers, and a case that proves
//! the fold survives a reload. A feature that needed a second copy of its state
//! would be a feature whose replay could lie.
//!
//! - [`todo`] is the task list the model maintains across steps.
//! - [`goal`] is the standing objective the session works toward.
//! - [`plan`] is the mode in which the model works out what it would do.
//! - [`feedback`] is what a run reports back to the operator.
//!
//! Parity: upstream `packages/todo`, `goal`, `plan`, `feedback`, `attachment`,
//! `workspace` and `skill`, restated against the tetanus seams that carry the
//! same decisions.

pub mod feedback;
pub mod goal;
pub mod plan;
pub mod todo;
