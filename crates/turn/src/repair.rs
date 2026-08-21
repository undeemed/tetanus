//! Crash repair: the closers an interrupted journal needs before it is safe to
//! resume.
//!
//! A process that dies mid-turn leaves a journal whose last turn never ended,
//! and possibly a tool call the model asked for and no result answers. Deriving
//! history from that log yields a dangling assistant tool call, which a
//! provider rejects. This module names that missing tail, and commits it:
//! nothing is repaired in memory that the journal does not record.
//!
//! Parity: upstream `packages/core/session` `interruptedTurnClosers`, pinned by
//! its `repair.spec.ts`.

use serde_json::json;
use tetanus_session::{SessionError, SessionEvent, SessionLog};

use crate::approval::ApprovalOutcome;
use crate::events::StopReason;
use crate::log::topic;

/// A tool call the crash cut off before the harness recorded it as started.
pub const TOOL_NOT_STARTED: &str = "TOOL_NOT_STARTED";
/// A tool call that had started: it may or may not have had an effect.
pub const TOOL_OUTCOME_UNKNOWN: &str = "TOOL_OUTCOME_UNKNOWN";

const NOT_STARTED_TEXT: &str = "The tool call was interrupted before the harness recorded it as \
     started. Retry it if it is still needed.";
const OUTCOME_UNKNOWN_TEXT: &str =
    "The tool call had started when the harness was interrupted, so \
     its outcome is unknown: retry only if the operation is read-only or idempotent, otherwise \
     first verify external state or ask the user.";

/// One event a repair appends. It has no `seq` or `time` yet: the log assigns
/// those when it commits, exactly as it does for a live append.
#[derive(Debug, Clone, PartialEq)]
pub struct Closer {
    pub ty: &'static str,
    pub data: serde_json::Value,
    /// Set on the synthesized `tool/result`, citing the `tool/call` it answers.
    pub sources: Option<Vec<u64>>,
}

/// The closers an interrupted journal is missing, in the order they must be
/// appended.
///
/// Empty for a balanced log, so calling this on a healthy journal is a no-op.
/// A result is synthesized only for a call of the still-open step: a call whose
/// step already closed is the driver's business, and an earlier committed turn
/// is settled.
pub fn interrupted_turn_closers(events: &[SessionEvent]) -> Vec<Closer> {
    let Some(turn_start) = last_index(events, topic::TURN_START) else {
        return Vec::new();
    };
    let open_turn = &events[turn_start..];
    if open_turn.iter().any(|e| e.ty == topic::TURN_END) {
        return Vec::new();
    }

    let turn = number(&open_turn[0], "turn");
    let steps = open_turn
        .iter()
        .filter(|e| e.ty == topic::STEP_START)
        .count() as u32;
    let open_step = last_index(open_turn, topic::STEP_START)
        .filter(|i| !open_turn[*i..].iter().any(|e| e.ty == topic::STEP_END));

    let mut closers = Vec::new();
    // An approval question the crash caught mid-flight is closed first, before
    // the result of the call it was about, because that is the order a live
    // turn writes them in: a decision precedes the call it decides. The scope
    // is the whole open turn and not the open step, because the pair is
    // turn-enclosed rather than step-enclosed - it carries no step to belong
    // to, for the same reason `tool/call` and `tool/result` carry none.
    for id in undecided_asks(open_turn) {
        closers.push(Closer {
            ty: topic::APPROVAL_DECIDED,
            data: json!({ "id": id, "outcome": ApprovalOutcome::Cancelled.as_str() }),
            sources: None,
        });
    }
    if let Some(step_start) = open_step {
        let step = &open_turn[step_start..];
        for call in unanswered_calls(step) {
            closers.push(synthesized_result(turn, call));
        }
        closers.push(Closer {
            ty: topic::STEP_END,
            data: json!({ "turn": turn, "step": number(&step[0], "step") }),
            sources: None,
        });
    }
    closers.push(Closer {
        ty: topic::TURN_END,
        data: json!({
            "turn": turn,
            "steps": steps,
            "stop_reason": StopReason::Interrupted.as_str(),
            "stop_veto": serde_json::Value::Null,
        }),
        sources: None,
    });
    closers
}

/// Append the closers an interrupted journal is missing, and return them as
/// committed events. A balanced journal is left untouched.
pub fn repair(log: &dyn SessionLog) -> Result<Vec<SessionEvent>, SessionError> {
    let mut written = Vec::new();
    for closer in interrupted_turn_closers(&log.events()) {
        let event = match closer.sources {
            Some(sources) => log.append_with_sources(closer.ty, closer.data, sources)?,
            None => log.append(closer.ty, closer.data)?,
        };
        written.push(event);
    }
    Ok(written)
}

/// The approval questions of the open turn that never got a decision, in the
/// order they were asked.
///
/// `cancelled` and not `unavailable` is what these are closed with: nobody was
/// found to be missing, the process holding the question died, and those are
/// different facts to a reader of the transcript. Both deny, so telling them
/// apart costs nothing and keeps the audit honest.
fn undecided_asks(open_turn: &[SessionEvent]) -> Vec<String> {
    let mut pending: Vec<String> = Vec::new();
    for event in open_turn {
        match event.ty.as_str() {
            topic::APPROVAL_ASKED => pending.push(string(&event.data, "id")),
            topic::APPROVAL_DECIDED => {
                let id = string(&event.data, "id");
                pending.retain(|open| *open != id);
            }
            _ => {}
        }
    }
    pending
}

/// A call the model asked for inside the open step, in the order it was asked.
struct Unanswered {
    id: String,
    name: String,
    /// The `tool/call` that recorded it as started, if the crash left one.
    started: Option<u64>,
}

fn unanswered_calls(step: &[SessionEvent]) -> Vec<Unanswered> {
    let mut pending: Vec<Unanswered> = Vec::new();
    for event in step {
        match event.ty.as_str() {
            // Only a call the assistant actually asked for can be answered; a
            // bare `tool/call` with no such request has nothing to answer.
            topic::ASSISTANT_MESSAGE => {
                let asked = event.data.get("tool_calls").and_then(|v| v.as_array());
                for call in asked.into_iter().flatten() {
                    pending.push(Unanswered {
                        id: string(call, "id"),
                        name: string(call, "name"),
                        started: None,
                    });
                }
            }
            topic::TOOL_CALL => {
                let id = string(&event.data, "id");
                if let Some(call) = pending.iter_mut().find(|c| c.id == id) {
                    call.started = Some(event.seq);
                }
            }
            topic::TOOL_RESULT => {
                let id = string(&event.data, "call_id");
                pending.retain(|call| call.id != id);
            }
            _ => {}
        }
    }
    pending
}

fn synthesized_result(turn: u64, call: Unanswered) -> Closer {
    let (code, content) = match call.started {
        Some(_) => (TOOL_OUTCOME_UNKNOWN, OUTCOME_UNKNOWN_TEXT),
        None => (TOOL_NOT_STARTED, NOT_STARTED_TEXT),
    };
    Closer {
        ty: topic::TOOL_RESULT,
        data: json!({
            "turn": turn,
            "call_id": call.id,
            "name": call.name,
            "ok": false,
            "content": content,
            "code": code,
        }),
        sources: call.started.map(|seq| vec![seq]),
    }
}

fn last_index(events: &[SessionEvent], ty: &str) -> Option<usize> {
    events.iter().rposition(|e| e.ty == ty)
}

fn number(event: &SessionEvent, key: &str) -> u64 {
    event.data[key].as_u64().unwrap_or_default()
}

fn string(value: &serde_json::Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}
