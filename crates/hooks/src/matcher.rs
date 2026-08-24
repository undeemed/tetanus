//! Which configured hooks a given event selects.
//!
//! Both hook dialects this workspace speaks put a *matcher* beside each hook:
//! a pattern that decides whether the hook fires for this tool name, this
//! session source, this whatever. The two dialects read the same pattern
//! differently, and that difference is the whole reason this is one module
//! rather than a call to a regex engine at each use site.
//!
//! - **Claude Code** treats a pattern of word characters and `|` as *literal
//!   alternatives*, and anything else as a regex. So `Bash` selects the tool
//!   named `Bash` and not `BashOutput`.
//! - **Codex** has no literal path: every non-empty pattern is an unanchored
//!   regex, so `Bash` is `/Bash/` and *does* select `BashOutput`.
//!
//! Absent, empty and `*` mean "everything" in both dialects.
//!
//! # Rust regexes are not JavaScript regexes
//!
//! Upstream compiles these patterns with `new RegExp`. This uses the `regex`
//! crate, which deliberately has no backreferences and no lookaround, because
//! it guarantees linear-time matching. A pattern using those features is valid
//! upstream and is reported invalid here.
//!
//! That is a widening of what [`matcher_diagnostic`] refuses, and it is the
//! safe direction: a hook whose matcher this cannot compile is refused at
//! configuration time with a message naming the pattern, rather than silently
//! never firing. It is recorded as a deliberate difference in
//! `docs/parity-updates/core-hook-matcher.md`.
//!
//! Parity: upstream `packages/hooks/hook-protocol/src/matcher.ts`, pinned by
//! its `matcher.spec.ts`.

use regex::Regex;

/// Which dialect's reading of a pattern to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatcherMode {
    /// Word-and-pipe patterns are literal alternatives; everything else is a regex.
    ClaudeCode,
    /// Every non-empty pattern is an unanchored regex.
    Codex,
}

impl MatcherMode {
    /// How the dialect names itself in a diagnostic. These strings are part of
    /// the message [`matcher_diagnostic`] returns, so they are upstream's.
    fn name(self) -> &'static str {
        match self {
            MatcherMode::ClaudeCode => "claude-code",
            MatcherMode::Codex => "codex",
        }
    }
}

/// Absent, empty and `*` all mean "select everything", in both dialects.
fn is_match_all(matcher: Option<&str>) -> bool {
    matches!(matcher, None | Some("") | Some("*"))
}

/// Whether a pattern is Claude's *literal* shape: word characters and `|` only.
///
/// This is the discriminator upstream spells `/^[A-Za-z0-9_|]+$/`. It is
/// checked by hand rather than with a regex because it is a character-class
/// test over a non-empty string, and saying so directly is cheaper to read
/// than compiling a pattern to answer it.
fn is_claude_literal(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '|')
}

/// Whether `matcher` selects `query` under `mode`.
///
/// An invalid pattern selects nothing rather than failing: matching happens
/// while a turn is running, and a bad matcher is a configuration mistake that
/// [`matcher_diagnostic`] is there to catch before it gets this far.
pub fn matches_matcher(matcher: Option<&str>, query: &str, mode: MatcherMode) -> bool {
    if is_match_all(matcher) {
        return true;
    }
    // Past the match-all guard, the matcher is a non-empty string.
    let pattern = matcher.unwrap_or_default();

    if mode == MatcherMode::ClaudeCode && is_claude_literal(pattern) {
        return pattern.split('|').any(|alternative| alternative == query);
    }
    Regex::new(pattern).is_ok_and(|regex| regex.is_match(query))
}

/// Judge one matcher before its hook group is accepted.
///
/// `None` means the matcher is usable. Otherwise the string is a diagnostic
/// naming the dialect and the pattern, so a deployment reading it can find the
/// line of configuration it came from.
pub fn matcher_diagnostic(matcher: Option<&str>, mode: MatcherMode) -> Option<String> {
    if is_match_all(matcher) {
        return None;
    }
    let pattern = matcher.unwrap_or_default();

    if mode == MatcherMode::ClaudeCode && is_claude_literal(pattern) {
        return None;
    }
    match Regex::new(pattern) {
        Ok(_) => None,
        Err(_) => Some(format!(
            "invalid {} regex matcher {}",
            mode.name(),
            quoted(pattern)
        )),
    }
}

/// The pattern as it appears in a diagnostic.
///
/// Upstream interpolates `JSON.stringify(pattern)`, so the message carries the
/// pattern in double quotes with the escaping JSON would use. A matcher is
/// almost always plain text, but a pattern containing a quote or a backslash
/// has to come back readable, which is what makes this worth a function.
fn quoted(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 2);
    out.push('"');
    for c in pattern.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
