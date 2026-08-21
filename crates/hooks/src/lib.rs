//! The out-of-process hooks protocol.
//!
//! A hook is a program a deployment configures to run at a named point in a
//! turn, in its own process, so it can observe what the agent is doing and in
//! some cases change the answer. This crate is the protocol: which hooks an
//! event selects, what is written to them, what they may write back, and how
//! several answers combine.
//!
//! Two dialects are spoken, Claude Code's and Codex's. They disagree in small
//! ways that are noted at each point they show, starting with [`matcher`].
//!
//! Parity: upstream `packages/hooks/*`.

pub mod codec;
pub mod events;
pub mod matcher;
pub mod merge;
pub mod types;

pub use codec::parse_hook_output;
pub use events::{append_hook_invoked, append_hook_result, summarize_stderr, HookDialect};
pub use matcher::{matcher_diagnostic, matches_matcher, MatcherMode};
pub use merge::merge_hook_outputs;
pub use types::{HookDecision, HookOutput, MergedDecision, MergedHookOutcome};
