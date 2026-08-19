//! A thin local stand-in for the engine<->presentation boundary.
//!
//! The contract (`docs/interface-contract.md` §4.3.1) fixes the payload of
//! every durable event type, and `tetanus-protocol` carries the same shapes as
//! Rust. Until that lands, this module holds the parts the timeline renders,
//! field for field, deliberately: swapping it for `tetanus-protocol` is then a
//! change of `use` lines, not a rewrite of the renderer.
//!
//! What is *not* here is the point of it. No engine crate, no I/O, no journal
//! reader. The renderer sees facts in this shape and nothing else, whether
//! they arrived from a journal on disk, an in-process call, or a WebSocket
//! frame.
//!
//! **Delete this file in M2**, together with the mapping in `main.rs` that
//! feeds it.

// The stub mirrors §4.3.1 field for field on purpose. A field the timeline
// does not read yet is still part of the shape it is written against, and
// dropping it would hide a rename when the real types arrive.
#![allow(dead_code)]

use serde::Deserialize;
use serde_json::Value;

/// One durable fact, exactly as the journal stores it. `ty` stays a free
/// string because the vocabulary grows, and a surface passes an unknown type
/// through instead of dropping it.
#[derive(Debug, Clone)]
pub struct SessionEvent {
    pub ty: String,
    pub seq: u64,
    pub data: Value,
}

impl SessionEvent {
    /// The typed payload of a type this build knows, per §4.3.1.
    ///
    /// `None` covers both a type this build does not know and a known type
    /// whose payload did not parse. A renderer treats them the same way: fall
    /// back to the raw event rather than dropping the line.
    pub fn parse(&self) -> Option<KnownEvent> {
        let mut tagged = self.data.clone();
        tagged
            .as_object_mut()?
            .insert("type".to_string(), Value::String(self.ty.clone()));
        serde_json::from_value(tagged).ok()
    }
}

/// The payload of a durable type this contract version knows.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum KnownEvent {
    #[serde(rename = "session/start")]
    SessionStart { model: String },
    #[serde(rename = "turn/start")]
    TurnStart { turn: u64 },
    #[serde(rename = "step/start")]
    StepStart { turn: u64, step: u32 },
    #[serde(rename = "user/message")]
    UserMessage { content: String },
    #[serde(rename = "assistant/chunk")]
    AssistantChunk {
        #[serde(flatten)]
        chunk: Chunk,
    },
    #[serde(rename = "assistant/message")]
    AssistantMessage {
        content: String,
        #[serde(default)]
        reasoning: String,
    },
    #[serde(rename = "tool/call")]
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    #[serde(rename = "tool/result")]
    ToolResult {
        /// The `tool/call.id` that asked for this. A surface pairs a result to
        /// its call by this id, never by arrival order - arrival order stops
        /// being pairing order the moment two calls are in flight.
        call_id: String,
        name: String,
        ok: bool,
        content: String,
    },
    #[serde(rename = "step/end")]
    StepEnd { turn: u64, step: u32 },
    #[serde(rename = "turn/end")]
    TurnEnd {
        turn: u64,
        steps: u32,
        stop_reason: StopReason,
        #[serde(default)]
        stop_veto: Option<String>,
    },
}

/// One piece of a provider stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "chunk", rename_all = "snake_case")]
pub enum Chunk {
    Text { delta: String },
    Reasoning { delta: String },
    ToolCall {},
}

/// Why a turn closed. `Other` is what lets the engine add a reason in a minor
/// version without a surface failing on it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopReason {
    Natural,
    PreStepRejected,
    MaxSteps,
    Cancelled,
    #[serde(untagged)]
    Other(String),
}

impl StopReason {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Natural => "natural",
            Self::PreStepRejected => "pre-step rejected",
            Self::MaxSteps => "step budget spent",
            Self::Cancelled => "cancelled",
            Self::Other(reason) => reason,
        }
    }
}

/// Test Design Specification: the stub boundary's parsing rule.
///
/// Features tested: that a known payload parses to its typed form, that an
/// unknown type and a malformed known payload both decline, and that a stop
/// reason this build does not know survives as text.
///
/// Rationale for testing a stub at all: these cases are the executable
/// statement of what §4.3.1 promises. When `tetanus-protocol` replaces this
/// file, the same cases move onto the real types, and a difference in
/// behaviour shows up as a failing case rather than as a wrong line on screen.
///
/// Environmental needs: none.
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(ty: &str, data: serde_json::Value) -> SessionEvent {
        SessionEvent {
            ty: ty.into(),
            seq: 0,
            data,
        }
    }

    /// TC-CLI-STUB-1: a payload of a type this build knows.
    /// Expected: the typed form, with the correlation id carried through.
    #[test]
    fn a_known_payload_parses_to_its_type() {
        let parsed = event(
            "tool/result",
            json!({ "call_id": "c1", "name": "echo", "ok": true, "content": "hi" }),
        )
        .parse();

        match parsed {
            Some(KnownEvent::ToolResult { call_id, ok, .. }) => {
                assert_eq!(call_id, "c1");
                assert!(ok);
            }
            other => panic!("{other:?}"),
        }
    }

    /// TC-CLI-STUB-2: a type this build does not know, and a known type whose
    /// payload does not fit.
    /// Expected: `None` for both. A renderer treats them the same way - show
    /// the raw event - so the two need not be told apart.
    #[test]
    fn an_unknown_type_and_a_malformed_payload_both_decline() {
        assert!(event("todo/write", json!({ "items": 3 })).parse().is_none());
        assert!(event("turn/start", json!({ "turn": "one" }))
            .parse()
            .is_none());
        assert!(event("turn/start", json!("not an object"))
            .parse()
            .is_none());
    }

    /// TC-CLI-STUB-3: a stop reason added after this build.
    /// Expected: it arrives as `Other` and renders as itself. This is what
    /// lets the engine add a reason in a minor version without a surface
    /// failing on it.
    #[test]
    fn an_unknown_stop_reason_survives_as_text() {
        let parsed = event(
            "turn/end",
            json!({ "turn": 1, "steps": 2, "stop_reason": "budget-exhausted" }),
        )
        .parse();

        match parsed {
            Some(KnownEvent::TurnEnd { stop_reason, .. }) => {
                assert_eq!(stop_reason.as_str(), "budget-exhausted");
            }
            other => panic!("{other:?}"),
        }
    }
}
