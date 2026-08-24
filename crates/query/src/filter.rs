//! What a caller may ask for, and what makes an ask malformed.

use serde::{Deserialize, Serialize};
use tetanus_protocol::rpc::{ErrorCode, RpcError};

/// Which side of the conversation an event belongs to.
///
/// Derived from the event type's domain - the part before the `/` - rather
/// than matched against a list of known types. The durable vocabulary grows,
/// and a role that had to be taught each new type would file `todo/write`
/// under "unknown" until someone remembered to extend the match. Filing it
/// under `Other("todo")` is both truthful and stable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// `session/start` and anything else the session header owns.
    Session,
    /// `turn/*` and `step/*`: the shape of the run, not its content.
    Control,
    User,
    Assistant,
    Tool,
    /// A domain this build has no name for, carrying that domain verbatim.
    #[serde(untagged)]
    Other(String),
}

impl Role {
    /// The role of an event of this type.
    pub fn of(ty: &str) -> Self {
        match ty.split('/').next().unwrap_or_default() {
            "session" => Self::Session,
            "turn" | "step" => Self::Control,
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "tool" | "tools" => Self::Tool,
            other => Self::Other(other.to_string()),
        }
    }
}

/// An inclusive range with either end open.
///
/// Inclusive on purpose: every bound a caller of this crate writes comes from
/// something a human said - "turns 3 through 5", "since yesterday" - and a
/// half-open range makes the reader subtract one to check it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bound<T> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<T>,
}

impl<T: Copy + PartialOrd> Bound<T> {
    pub fn all() -> Self {
        Self {
            min: None,
            max: None,
        }
    }

    pub fn exactly(value: T) -> Self {
        Self {
            min: Some(value),
            max: Some(value),
        }
    }

    pub fn from(min: T) -> Self {
        Self {
            min: Some(min),
            max: None,
        }
    }

    pub fn through(max: T) -> Self {
        Self {
            min: None,
            max: Some(max),
        }
    }

    pub fn span(min: T, max: T) -> Self {
        Self {
            min: Some(min),
            max: Some(max),
        }
    }

    pub fn contains(&self, value: T) -> bool {
        self.min.is_none_or(|min| value >= min) && self.max.is_none_or(|max| value <= max)
    }

    /// True when the range can never match: `min` above `max`.
    fn inverted(&self) -> bool {
        match (self.min, self.max) {
            (Some(min), Some(max)) => min > max,
            _ => false,
        }
    }
}

/// Every predicate one selection may carry.
///
/// Clauses are ANDed with each other and ORed within themselves, which is the
/// only combination a caller never has to read twice: "a `tool/result` from
/// `echo` or `read`, in turns 2 through 4".
///
/// Every list clause is an [`Option`], and the two states mean different
/// things. `None` is "do not ask about this at all"; `Some(vec![])` is "ask,
/// and accept nothing", which matches no event. A surface that builds a filter
/// out of a user's selection needs that: an empty selection means the user
/// picked nothing, and answering it with every event in the log would be the
/// most expensive way possible to be wrong.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventFilter {
    /// Event types, matched exactly (`"tool/call"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<Role>>,
    /// Tool names. Matches a `tool/call` or `tool/result` naming one of them,
    /// and nothing else: an event with no tool is not a tool this asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Turn numbers as derived by the fold. An event outside every turn -
    /// `session/start` - matches no turn range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<Bound<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Bound<u32>>,
    /// Unix epoch milliseconds, as `SessionEvent.time` records them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<Bound<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<Bound<u64>>,
    /// A `tool/result` outcome. Matches only events that have one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// A literal, case-insensitive substring of the event's text.
    ///
    /// Deliberately literal and deliberately not a regular expression: this is
    /// a scan over events already in memory, and a caller who typed `a.b` meant
    /// `a.b`. Full-text search is a backend's job and this crate has no
    /// backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl EventFilter {
    /// Refuse a filter that cannot be answered, before it is answered with an
    /// empty page that reads like a fact about the session.
    ///
    /// An inverted range is the whole of it. An empty list is legal - it means
    /// "nothing" and is answered with nothing.
    pub fn validate(&self) -> Result<(), QueryError> {
        let inverted = [
            self.turns.map(|b| (b.inverted(), "turns")),
            self.steps.map(|b| (b.inverted(), "steps")),
            self.time.map(|b| (b.inverted(), "time")),
            self.seq.map(|b| (b.inverted(), "seq")),
        ];
        for (bad, field) in inverted.into_iter().flatten() {
            if bad {
                return Err(QueryError::InvalidFilter(format!(
                    "`{field}` has a minimum above its maximum, so it can never match"
                )));
            }
        }
        Ok(())
    }

    // Builder methods. A filter is usually one or two clauses, and naming them
    // at the call site is what keeps a reader from counting positional
    // arguments.

    pub fn types(mut self, types: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.types = Some(types.into_iter().map(Into::into).collect());
        self
    }

    pub fn roles(mut self, roles: impl IntoIterator<Item = Role>) -> Self {
        self.roles = Some(roles.into_iter().collect());
        self
    }

    pub fn tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    pub fn turns(mut self, turns: Bound<u64>) -> Self {
        self.turns = Some(turns);
        self
    }

    pub fn steps(mut self, steps: Bound<u32>) -> Self {
        self.steps = Some(steps);
        self
    }

    pub fn time(mut self, time: Bound<u64>) -> Self {
        self.time = Some(time);
        self
    }

    pub fn seq(mut self, seq: Bound<u64>) -> Self {
        self.seq = Some(seq);
        self
    }

    pub fn ok(mut self, ok: bool) -> Self {
        self.ok = Some(ok);
        self
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }
}

/// Why a query could not be answered.
///
/// Four cases, not a free string, because a caller behind a carrier has to map
/// each to a code and a caller in process wants to match on them. Every one
/// converts to an [`RpcError`] with the code the contract's error table
/// already assigns that meaning, so a query served over the wire reports what
/// the rest of the contract reports.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryError {
    /// The ask itself is malformed - an inverted range.
    InvalidFilter(String),
    /// A page or window whose size is outside what this build serves.
    InvalidWindow(String),
    /// No session by that id.
    NotFound(String),
    /// The source refused. Carried whole, because the source's code is more
    /// specific than anything this crate could invent for it.
    Source(RpcError),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFilter(why) | Self::InvalidWindow(why) => f.write_str(why),
            Self::NotFound(id) => write!(f, "no session `{id}`"),
            Self::Source(error) => f.write_str(&error.message),
        }
    }
}

impl std::error::Error for QueryError {}

impl From<RpcError> for QueryError {
    fn from(error: RpcError) -> Self {
        Self::Source(error)
    }
}

impl From<QueryError> for RpcError {
    fn from(error: QueryError) -> Self {
        match error {
            QueryError::Source(error) => error,
            QueryError::NotFound(ref id) => {
                RpcError::new(ErrorCode::SessionNotFound, error.to_string())
                    .with_data(serde_json::json!({ "session_id": id }))
            }
            other => RpcError::new(ErrorCode::InvalidParams, other.to_string()),
        }
    }
}
