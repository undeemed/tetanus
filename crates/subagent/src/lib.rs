//! Delegation: an agent that starts another agent.
//!
//! A subagent is a child agent a parent starts to do a bounded piece of work.
//! The parent may wait for it, or leave it running and collect the answer
//! later; the child may itself delegate, which is why the first thing this
//! crate defines is the budget that stops that recursion.
//!
//! Parity: upstream `packages/subagent/*`.

pub mod depth;
