//! Reading Codex's hook configuration.
//!
//! The same job as [`crate::claude_code`], for the other dialect, and the
//! differences are all deliberate rather than incidental:
//!
//! - **Five events, not seven.** An event Codex itself defines but this
//!   adapter does not serve is dropped like any unknown one, because serving
//!   half of it would be worse than not serving it.
//! - **No substitution.** Codex expands nothing at parse time; a `${VAR}` in a
//!   command is the shell's business later, and rewriting it here would change
//!   a command the deployment wrote.
//! - **`async: true` is refused, and recorded.** This adapter runs a hook and
//!   waits for its answer. A hook that asked to run in the background would
//!   have its answer arrive after the decision it was meant to inform, so
//!   running it as if it were synchronous would silently change its meaning.
//! - **Two spellings for the timeout**, `timeout` and `timeoutSec`, because
//!   Codex accepts both.
//!
//! The shape rules match the other dialect, and for the same reasons: lenient
//! about malformed entries so one bad stanza cannot stop a boot, strict about
//! a matcher on a runnable group so a hook that could never fire is refused
//! rather than silently dead.
//!
//! Parity: upstream `packages/hooks/hooks-codex/src/config.ts`, pinned by its
//! `config.spec.ts`.

use serde_json::Value;

use crate::claude_code::{ConfigError, MatcherGroup};
use crate::matcher::{matcher_diagnostic, MatcherMode};
use crate::runner::CommandHook;

/// The five hook points this adapter serves.
pub const CODEX_EVENTS: [&str; 5] = [
    "PreToolUse",
    "PostToolUse",
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
];

/// Events with nothing for a matcher to match against.
const EVENTS_WITHOUT_A_SUBJECT: [&str; 2] = ["UserPromptSubmit", "Stop"];

/// A hook this adapter recognised and will not run, with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedHook {
    /// The event it was configured on.
    pub event: String,
    /// Why it will not run, in words a deployment can act on.
    pub reason: String,
}

/// What one Codex configuration file turned out to hold.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedCodexConfig {
    /// Runnable groups, by event name.
    pub config: Vec<(String, Vec<MatcherGroup>)>,
    /// Hooks that were recognised but will not run.
    pub skipped: Vec<SkippedHook>,
}

impl ParsedCodexConfig {
    /// The groups configured for one event.
    pub fn event(&self, event: &str) -> Option<&[MatcherGroup]> {
        self.config
            .iter()
            .find(|(name, _)| name == event)
            .map(|(_, groups)| groups.as_slice())
    }
}

/// Read a Codex configuration document.
///
/// Accepts either a `hooks` wrapper or the bare event map.
pub fn parse_codex_config(raw: &Value) -> Result<ParsedCodexConfig, ConfigError> {
    let mut parsed = ParsedCodexConfig::default();

    let Some(root) = raw.as_object() else {
        return Ok(parsed);
    };
    let hooks_map = root.get("hooks").and_then(Value::as_object).unwrap_or(root);

    for event in CODEX_EVENTS {
        let Some(raw_groups) = hooks_map.get(event).and_then(Value::as_array) else {
            continue;
        };
        let mut groups = Vec::new();
        for raw_group in raw_groups {
            if let Some(group) = read_group(event, raw_group, &mut parsed.skipped)? {
                groups.push(group);
            }
        }
        if !groups.is_empty() {
            parsed.config.push((event.to_owned(), groups));
        }
    }

    Ok(parsed)
}

/// One matcher group, or `None` when nothing in it can run.
fn read_group(
    event: &str,
    raw_group: &Value,
    skipped: &mut Vec<SkippedHook>,
) -> Result<Option<MatcherGroup>, ConfigError> {
    let Some(group) = raw_group.as_object() else {
        return Ok(None);
    };
    let Some(raw_hooks) = group.get("hooks").and_then(Value::as_array) else {
        return Ok(None);
    };

    let mut hooks = Vec::new();
    for raw_hook in raw_hooks {
        if let Some(hook) = read_hook(event, raw_hook, skipped) {
            hooks.push(hook);
        }
    }
    // Emptiness before the matcher, so a group that could never fire does not
    // fail the boot over a pattern that could not have mattered.
    if hooks.is_empty() {
        return Ok(None);
    }

    let matcher = if EVENTS_WITHOUT_A_SUBJECT.contains(&event) {
        None
    } else {
        group
            .get("matcher")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };

    if let Some(diagnostic) = matcher_diagnostic(matcher.as_deref(), MatcherMode::Codex) {
        return Err(ConfigError(format!("{diagnostic} on event \"{event}\"")));
    }

    Ok(Some(MatcherGroup { matcher, hooks }))
}

/// One hook, or `None` when it is malformed or will not be run.
fn read_hook(event: &str, raw_hook: &Value, skipped: &mut Vec<SkippedHook>) -> Option<CommandHook> {
    let hook = raw_hook.as_object()?;

    let ty = hook
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("command");
    if ty != "command" {
        skipped.push(SkippedHook {
            event: event.to_owned(),
            reason: format!("unsupported \"{ty}\" hook"),
        });
        return None;
    }

    // A background hook's answer would arrive after the decision it was meant
    // to inform, so running it as if it were synchronous would change what it
    // means. Refused, and recorded so the deployment is told.
    if hook.get("async") == Some(&Value::Bool(true)) {
        skipped.push(SkippedHook {
            event: event.to_owned(),
            reason: "async hook".to_owned(),
        });
        return None;
    }

    let command = hook.get("command").and_then(Value::as_str)?;
    Some(CommandHook {
        // No substitution: a `${VAR}` here is the shell's later, and rewriting
        // it would change the command the deployment wrote.
        command: command.to_owned(),
        timeout_sec: hook
            .get("timeout")
            .and_then(Value::as_u64)
            .or_else(|| hook.get("timeoutSec").and_then(Value::as_u64)),
    })
}
