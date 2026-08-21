//! Reading Claude Code's hook configuration.
//!
//! A deployment writes hooks in Claude Code's own format — an event name
//! mapping to matcher groups, each holding hooks. This turns that into the
//! shared shape the runner takes.
//!
//! # Two failure policies, on purpose
//!
//! Parsing is lenient about *shape* and strict about *matchers*, and the split
//! is deliberate:
//!
//! - A malformed entry is dropped, never fatal. A settings file is edited by
//!   hand, and one bad stanza must not stop the harness booting — the hook
//!   that stanza described simply does not run.
//! - An uncompilable matcher on a runnable group **is** fatal. That group's
//!   hooks would never fire, silently, and a hook a deployment believes is
//!   guarding something but which cannot match is worse than no hook at all.
//!   Failing at parse time is what turns it into a message naming the event.
//!
//! Parity: upstream `packages/hooks/hooks-claude-code/src/config.ts`, pinned
//! by its `config.spec.ts`.

use serde_json::{Map, Value};

use crate::matcher::{matcher_diagnostic, MatcherMode};
use crate::runner::CommandHook;

/// The events this dialect defines. An event outside this list is ignored
/// entirely, before its groups are looked at.
const CLAUDE_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "SubagentStart",
    "SubagentStop",
];

/// Events with nothing for a matcher to match against.
///
/// A matcher on one of these is discarded rather than refused, because it
/// constrains nothing and refusing it would fail a boot over a field that
/// could not have had an effect either way.
const EVENTS_WITHOUT_A_SUBJECT: [&str; 2] = ["UserPromptSubmit", "Stop"];

/// One matcher and the hooks it selects.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MatcherGroup {
    /// The pattern, absent for a match-all group.
    pub matcher: Option<String>,
    /// The hooks that run when it matches.
    pub hooks: Vec<CommandHook>,
}

/// A hook this dialect defines but this harness does not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedHook {
    /// The event it was configured on.
    pub event: String,
    /// The hook type that was skipped.
    pub ty: String,
}

/// What one configuration file turned out to hold.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedClaudeConfig {
    /// Runnable groups, by event name.
    pub config: Vec<(String, Vec<MatcherGroup>)>,
    /// Hooks that were recognised but cannot be run, so a caller can say so.
    pub skipped: Vec<SkippedHook>,
}

impl ParsedClaudeConfig {
    /// The groups configured for one event.
    pub fn event(&self, event: &str) -> Option<&[MatcherGroup]> {
        self.config
            .iter()
            .find(|(name, _)| name == event)
            .map(|(_, groups)| groups.as_slice())
    }
}

/// The values substituted into a command as it is read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubstitutionVars {
    /// Replaces `${CLAUDE_PLUGIN_ROOT}`.
    pub plugin_root: Option<String>,
    /// Replaces `${CLAUDE_PROJECT_DIR}`.
    pub project_dir: Option<String>,
}

/// A matcher that cannot be compiled, on a group that would otherwise run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// Replace every occurrence of each token whose value is set.
///
/// A token whose variable is unset is left verbatim rather than replaced with
/// an empty string: `${CLAUDE_PLUGIN_ROOT}/x.sh` becoming `/x.sh` would point
/// at a real and quite different path.
pub fn substitute_command(command: &str, vars: &SubstitutionVars) -> String {
    let mut out = command.to_owned();
    if let Some(root) = &vars.plugin_root {
        out = out.replace("${CLAUDE_PLUGIN_ROOT}", root);
    }
    if let Some(dir) = &vars.project_dir {
        out = out.replace("${CLAUDE_PROJECT_DIR}", dir);
    }
    out
}

/// Read a configuration document.
///
/// Accepts either a settings file with a `hooks` key or the bare event map,
/// because both are things a deployment actually writes.
pub fn parse_claude_code_config(
    raw: &Value,
    vars: &SubstitutionVars,
) -> Result<ParsedClaudeConfig, ConfigError> {
    let mut parsed = ParsedClaudeConfig::default();

    let Some(root) = raw.as_object() else {
        return Ok(parsed);
    };
    // A settings file nests them under `hooks`; a `hooks.json` is the map itself.
    let hooks_map = root.get("hooks").and_then(Value::as_object).unwrap_or(root);

    for event in CLAUDE_EVENTS {
        let Some(raw_groups) = hooks_map.get(event).and_then(Value::as_array) else {
            continue;
        };
        let mut groups = Vec::new();
        for raw_group in raw_groups {
            if let Some(group) = read_group(event, raw_group, vars, &mut parsed.skipped)? {
                groups.push(group);
            }
        }
        if !groups.is_empty() {
            parsed.config.push((event.to_owned(), groups));
        }
    }

    Ok(parsed)
}

/// One matcher group, or `None` when there is nothing runnable in it.
fn read_group(
    event: &str,
    raw_group: &Value,
    vars: &SubstitutionVars,
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
        if let Some(hook) = read_hook(event, raw_hook, vars, skipped) {
            hooks.push(hook);
        }
    }
    // A group with nothing runnable in it is not a group. Checked before the
    // matcher is validated, so a bad matcher on a group that could never have
    // run does not fail the boot.
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

    if let Some(diagnostic) = matcher_diagnostic(matcher.as_deref(), MatcherMode::ClaudeCode) {
        return Err(ConfigError(format!("{diagnostic} on event \"{event}\"")));
    }

    Ok(Some(MatcherGroup { matcher, hooks }))
}

/// One hook, or `None` when it is malformed. A recognised type this harness
/// cannot run is recorded rather than dropped silently.
fn read_hook(
    event: &str,
    raw_hook: &Value,
    vars: &SubstitutionVars,
    skipped: &mut Vec<SkippedHook>,
) -> Option<CommandHook> {
    let hook: &Map<String, Value> = raw_hook.as_object()?;

    // An absent type means `command`: that is this dialect's default, and a
    // deployment writes hooks without one.
    let ty = hook
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("command");
    if ty != "command" {
        skipped.push(SkippedHook {
            event: event.to_owned(),
            ty: ty.to_owned(),
        });
        return None;
    }

    let command = hook.get("command").and_then(Value::as_str)?;
    Some(CommandHook {
        command: substitute_command(command, vars),
        // Seconds on the wire, as everywhere in this dialect's config.
        timeout_sec: hook.get("timeout").and_then(Value::as_u64),
    })
}
