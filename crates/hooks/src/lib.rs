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

pub mod claude_code;
pub mod codec;
pub mod detached;
pub mod events;
pub mod invariant;
pub mod matcher;
pub mod merge;
pub mod runner;
pub mod types;

pub use claude_code::{parse_claude_code_config, MatcherGroup};
pub use codec::parse_hook_output;
pub use detached::{CancelSignal, DetachedRuns};
pub use events::{append_hook_invoked, append_hook_result, summarize_stderr, HookDialect};
pub use invariant::hook_stream_faults;
pub use matcher::{matcher_diagnostic, matches_matcher, MatcherMode};
pub use merge::merge_hook_outputs;
pub use runner::{run_hook, CommandHook, HookExecutor, DEFAULT_HOOK_TIMEOUT_MS};
pub use types::{HookDecision, HookOutput, MergedDecision, MergedHookOutcome};
