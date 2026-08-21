//! The environment layer: settings a deployment supplies without a file.
//!
//! [`Layer::Env`](crate::Layer) has existed since the layered config did, and
//! nothing ever filled it. The documented stack - defaults, then the file,
//! then the environment, then flags - was three layers with a hole in the
//! middle, so a container or a CI job had no way to set a value short of
//! writing a document, and `tetanus config` could never report `env` as a
//! provenance because no key was ever set there.
//!
//! **`__` separates path segments; a single `_` is part of a key.** Every
//! settled key is sectioned and several are multi-word, so one separator
//! cannot serve both jobs: `agent.max_parallel_tool_calls` and
//! `agent.max.parallel.tool.calls` would be indistinguishable.
//! `TETANUS_AGENT__MAX_PARALLEL_TOOL_CALLS` is unambiguous, and reads as the
//! key it sets.
//!
//! **A variable with no `__` sets nothing.** Every key this workspace settles
//! lives in a section, so a bare `TETANUS_SOMETHING` names no key that could
//! exist. It also keeps `TETANUS_HOME` what it is - the harness home, read by
//! [`crate::home`] - rather than quietly becoming a setting called `home`.
//!
//! **A value is JSON when it parses as JSON, and text otherwise.** A reader
//! that took every value as text would make `TETANUS_AGENT__MAX_STEPS=8` fail
//! type resolution with "must be an integer, not a string", which is a
//! confusing way to be told the mechanism does not work. The cost is that a
//! value which happens to look like JSON is read as JSON, so a model literally
//! named `123` needs quoting: `TETANUS_MODEL__DEFAULT='"123"'`. That is the
//! usual bargain, and the quoting escape is pinned by a case rather than left
//! to be discovered.

use crate::Document;

/// The prefix a variable needs before it is considered at all.
pub const PREFIX: &str = "TETANUS_";

/// What separates one path segment from the next.
pub const SEPARATOR: &str = "__";

/// Read this process's environment into a config layer.
pub fn from_env() -> Document {
    from_vars(std::env::vars())
}

/// The same, over variables a caller supplies.
///
/// This is the tested surface. Reading the real environment in a test means
/// mutating global state that every other case in the binary shares, so the
/// rule is checked here and `from_env` is the one line that reaches for
/// `std::env`.
pub fn from_vars<I, K, V>(vars: I) -> Document
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut document = Document::new();
    for (name, value) in vars {
        if let Some(key) = key_of(name.as_ref()) {
            document.insert(key, value_of(value.as_ref()));
        }
    }
    document
}

/// The dotted key a variable names, or `None` when it names none.
fn key_of(name: &str) -> Option<String> {
    let rest = name.strip_prefix(PREFIX)?;
    if !rest.contains(SEPARATOR) {
        return None;
    }
    let segments: Vec<String> = rest
        .split(SEPARATOR)
        .map(|segment| segment.to_lowercase())
        .collect();
    // An empty segment means a doubled separator or a trailing one, which
    // names a key with an empty part - `llm..retry` matches nothing and is
    // more likely a typo than an intent.
    if segments.iter().any(String::is_empty) {
        return None;
    }
    Some(segments.join("."))
}

/// What a variable's text means as a value.
fn value_of(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.to_string()))
}
