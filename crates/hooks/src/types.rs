//! What a hook may say when it answers.
//!
//! One hook is one process. It is written a description of the point it fired
//! at, and what it prints back is decoded into a [`HookOutput`]. This module
//! is that vocabulary, shared by both dialects; the dialect-specific decoding
//! lives with each adapter.
//!
//! Parity: upstream `packages/hooks/hook-protocol/src/types.ts`.

/// A permission answer as a hook wrote it.
///
/// Two dialects and two spellings reach the same three meanings. Upstream
/// keeps all five spellings rather than normalising at the edge, because the
/// spelling a hook used is part of what it said, and [`HookDecision::merged`]
/// is the single place the collapse happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookDecision {
    /// Claude Code's permitting spelling.
    Approve,
    /// The `permissionDecision` permitting spelling.
    Allow,
    /// Claude Code's forbidding spelling.
    Block,
    /// The `permissionDecision` forbidding spelling.
    Deny,
    /// Ask the user. Arises only from a `permissionDecision`.
    Ask,
}

impl HookDecision {
    /// The meaning behind the spelling.
    pub fn merged(self) -> MergedDecision {
        match self {
            HookDecision::Approve | HookDecision::Allow => MergedDecision::Allow,
            HookDecision::Ask => MergedDecision::Ask,
            HookDecision::Block | HookDecision::Deny => MergedDecision::Deny,
        }
    }
}

/// What a whole hook point resolved to.
///
/// The variants are declared least-restrictive first, and the derived ordering
/// is the precedence rule: merging is `max`, so "most restrictive wins" is a
/// property of the type rather than a comparison someone has to keep correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum MergedDecision {
    /// No hook expressed a permission answer.
    #[default]
    None,
    /// Permitted.
    Allow,
    /// Ask the user.
    Ask,
    /// Forbidden.
    Deny,
}

/// One hook's decoded answer.
///
/// Every field beyond the process result is optional because a hook that only
/// wanted to observe says nothing at all, and that is the common case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookOutput {
    /// The process's exit status.
    pub exit_code: i32,
    /// What it wrote to stdout.
    pub stdout: String,
    /// What it wrote to stderr.
    pub stderr: String,
    /// Its permission answer, if it gave one.
    pub decision: Option<HookDecision>,
    /// Why, shown when the answer forbids or asks.
    pub reason: Option<String>,
    /// Whether the turn should go on. `Some(false)` is a request to halt, and
    /// pairs with [`HookOutput::stop_reason`]. `Some(true)` and `None` proceed.
    ///
    /// The wire name is `continue`, which is a Rust keyword.
    pub proceed: Option<bool>,
    /// Why the turn was asked to halt.
    pub stop_reason: Option<String>,
    /// Text for the model, added to the turn's context.
    pub additional_context: Option<String>,
    /// A warning for the user.
    pub system_message: Option<String>,
}

/// Every matched hook's answer at one point, folded into one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergedHookOutcome {
    /// The most restrictive permission answer any hook gave.
    pub decision: MergedDecision,
    /// The reasons behind [`MergedHookOutcome::decision`], joined by a blank
    /// line. Only the winning answer's reasons appear here.
    pub reason: Option<String>,
    /// Whether any hook asked to halt.
    pub stop: bool,
    /// The reason given by the *first* hook that asked to halt.
    pub stop_reason: Option<String>,
    /// Every hook's context text, in hook order. Not joined: what separates
    /// them is the caller's decision, not this fold's.
    pub additional_context: Vec<String>,
    /// Every hook's warning, in hook order.
    pub system_messages: Vec<String>,
}
