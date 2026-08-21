//! The durable record that a hook ran, and what it decided.
//!
//! Hooks are configured by a deployment and run other people's programs inside
//! a turn. What they did has to be auditable afterwards, which is what these
//! two events are for. They are *log-only*: nothing a client renders depends
//! on them, and nothing in a turn reads them back. They exist so that "why was
//! my tool call denied" has an answer on the journal.
//!
//! The two are a pair, correlated by `handler_id`, and both live inside the
//! open turn. Writing them here rather than in each adapter is deliberate:
//! the event's meaning belongs to the protocol that declares it, so the two
//! dialects cannot drift on what a `hook/result` means.
//!
//! Parity: upstream `packages/hooks/hook-protocol/src/events.ts`, pinned by
//! its `events.spec.ts`.

use serde_json::{json, Map, Value};
use tetanus_session::{SessionError, SessionEvent, SessionLog};

use crate::types::{HookDecision, HookOutput};

/// The cap both dialects default to for a recorded stderr summary.
///
/// It lives here, once, beside the truncation rule it bounds, so the adapters
/// cannot drift apart on the shared event's default.
pub const DEFAULT_STDERR_SUMMARY_MAX_CHARS: usize = 500;

/// Which bridge ran a hook.
///
/// The same two words as [`crate::MatcherMode`], and deliberately a separate
/// type: this one is written to the journal, so it is a durable wire value,
/// while the matcher mode is an internal switch. They are free to diverge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookDialect {
    /// The Claude Code bridge.
    ClaudeCode,
    /// The Codex bridge.
    Codex,
}

impl HookDialect {
    /// The word that goes on the journal.
    pub fn as_str(self) -> &'static str {
        match self {
            HookDialect::ClaudeCode => "claude-code",
            HookDialect::Codex => "codex",
        }
    }

    /// How this bridge reads a matcher pattern.
    pub fn matcher_mode(self) -> crate::MatcherMode {
        match self {
            HookDialect::ClaudeCode => crate::MatcherMode::ClaudeCode,
            HookDialect::Codex => crate::MatcherMode::Codex,
        }
    }
}

/// What identifies one hook invocation across its pair of events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInvocation {
    /// The open turn this ran inside.
    pub turn: u64,
    /// The hook point, such as `PreToolUse` or `Stop`.
    pub point: String,
    /// The bridge that ran it.
    pub dialect: HookDialect,
    /// Correlates this invocation with its result.
    pub handler_id: String,
    /// The pattern that selected this hook. Absent for a match-all hook, and
    /// omitted from the payload rather than written as null, because "matched
    /// everything" and "matched this pattern" are different facts.
    pub matcher: Option<String>,
}

/// The decided half of the pair.
#[derive(Debug, Clone, PartialEq)]
pub struct HookResultRecord {
    /// The open turn this ran inside.
    pub turn: u64,
    /// The hook point.
    pub point: String,
    /// Correlates this result with its invocation.
    pub handler_id: String,
    /// What the hook produced. The durable fields are derived from it here, so
    /// the semantics live with the event rather than in each adapter.
    pub output: HookOutput,
    /// The cap for the recorded stderr summary. Passed in rather than read
    /// from a constant, because the bound is the adapter's configuration to
    /// own; [`DEFAULT_STDERR_SUMMARY_MAX_CHARS`] is the reference default.
    pub stderr_summary_max_chars: usize,
    /// How long the run took, for the audit trail.
    pub duration_ms: u64,
}

/// Trim a hook's stderr down to what the journal keeps.
///
/// Blank stderr is `None` rather than an empty string: a hook that printed
/// nothing has no summary, and an empty key on the event would suggest it did.
///
/// # Characters, not bytes, and not UTF-16 units
///
/// The cap counts characters. Upstream's `String.prototype.slice` counts
/// UTF-16 code units, so a summary of astral-plane text is capped at a
/// different point there. Slicing by *bytes* would be worse than either - it
/// panics mid-character, which is the defect the tool-result pruner's port
/// already had to fix - so this counts what a reader would count.
pub fn summarize_stderr(stderr: &str, max_chars: usize) -> Option<String> {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Counting is bounded by the cap: a hook that printed a megabyte of stderr
    // should not cost a full pass to discover it is over.
    if trimmed.chars().nth(max_chars).is_none() {
        return Some(trimmed.to_owned());
    }
    let head: String = trimmed.chars().take(max_chars).collect();
    Some(format!("{head}…"))
}

/// Record that a hook was invoked.
pub fn append_hook_invoked(
    log: &dyn SessionLog,
    invocation: &HookInvocation,
) -> Result<SessionEvent, SessionError> {
    let mut data = Map::new();
    data.insert("turn".into(), json!(invocation.turn));
    data.insert("point".into(), json!(invocation.point));
    data.insert("dialect".into(), json!(invocation.dialect.as_str()));
    data.insert("handlerId".into(), json!(invocation.handler_id));
    if let Some(matcher) = &invocation.matcher {
        data.insert("matcher".into(), json!(matcher));
    }
    log.append("hook/invoked", Value::Object(data))
}

/// Record what a hook decided, paired with its invocation by `handler_id`.
pub fn append_hook_result(
    log: &dyn SessionLog,
    record: &HookResultRecord,
) -> Result<SessionEvent, SessionError> {
    let mut data = Map::new();
    data.insert("turn".into(), json!(record.turn));
    data.insert("point".into(), json!(record.point));
    data.insert("handlerId".into(), json!(record.handler_id));
    data.insert("decision".into(), json!(recorded_decision(&record.output)));
    if let Some(exit_code) = record.output.exit_code {
        data.insert("exitCode".into(), json!(exit_code));
    }
    if let Some(summary) = summarize_stderr(&record.output.stderr, record.stderr_summary_max_chars)
    {
        data.insert("stderrSummary".into(), json!(summary));
    }
    data.insert("durationMs".into(), json!(record.duration_ms));
    log.append("hook/result", Value::Object(data))
}

/// The word the journal records for what a hook decided.
///
/// A hook that expressed a permission answer is recorded as that answer. One
/// that only asked to halt is recorded as `stop`, and one that did neither as
/// `pass` - so every result carries a decision and a reader never has to infer
/// "nothing happened" from an absent field.
fn recorded_decision(output: &HookOutput) -> &'static str {
    match output.decision {
        Some(HookDecision::Approve) => "approve",
        Some(HookDecision::Allow) => "allow",
        Some(HookDecision::Block) => "block",
        Some(HookDecision::Deny) => "deny",
        Some(HookDecision::Ask) => "ask",
        None if output.proceed == Some(false) => "stop",
        None => "pass",
    }
}
