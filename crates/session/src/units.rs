//! The projection units this crate can fold on its own: the two whose answer
//! is a fact of the journal's shape rather than of what a model charges.
//!
//! [`Title`] is what to call a session and [`Stats`] is how much work it did.
//! Neither needs to price anything, so neither needs the model seam, and both
//! live here rather than in `crates/turn` - a listing that wants a title must
//! not have to link a provider adapter to get one. The three priced units
//! (`tetanus_turn::projections`) do need it, and live there.
//!
//! Both follow the unit contract of [`crate::projection`]: pure, synchronous,
//! JSON state, no clock and no subscription. What that buys is in that
//! module's own header; the short version is that a stored value is always a
//! shortcut and never an authority.
//!
//! Parity: upstream `packages/session/session-title-first-prompt` (its
//! deterministic titler; the LLM titlers are a phase ② surface) and
//! `packages/session/session-stats`, pinned by their unit specs.

use serde_json::{json, Value};

use crate::projection::Projection;
use crate::SessionEvent;

/// The key [`Title`] serves under.
pub const TITLE: &str = "session.title";
/// The key [`Stats`] serves under.
pub const STATS: &str = "session.stats";

/// Longest title this unit reports, in characters. A picker gets a line, not a
/// paragraph; the whole message is one page of the journal away.
pub const MAX_TITLE: usize = 80;

/// What to call a session: its first user message, cut to one line.
///
/// The *first*, not the newest, because a title that moved every turn would
/// make a session unfindable in a list a user is scanning. A message that is
/// nothing but whitespace is not a title, for the same reason it is not a
/// prompt.
///
/// Upstream's shipped titler asks a model. This one is deterministic, which is
/// what lets it be a projection at all: a fold that called a provider would
/// give a different answer on replay, and the checkpoint would be a cache of
/// something unreproducible.
#[derive(Debug, Default)]
pub struct Title;

impl Projection for Title {
    fn key(&self) -> &str {
        TITLE
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn init(&self) -> Value {
        Value::Null
    }

    fn apply(&self, state: Value, event: &SessionEvent) -> Value {
        // Settled at the first user message, so every later one folds to the
        // state it was given and the whole tail of a long session is free.
        if !state.is_null() || event.ty != "user/message" {
            return state;
        }
        match title_of(event.data.get("content").and_then(Value::as_str)) {
            Some(title) => Value::String(title),
            None => state,
        }
    }

    fn view(&self, state: &Value) -> Value {
        state.clone()
    }
}

/// One message as a title, or `None` when it does not make one.
pub fn title_of(content: Option<&str>) -> Option<String> {
    let content = content?.trim();
    if content.is_empty() {
        return None;
    }
    let line = content.lines().next().unwrap_or(content);
    // Cut by characters and not by bytes: a title is text a person reads, and
    // slicing a UTF-8 string at a byte offset panics mid-character.
    match line.char_indices().nth(MAX_TITLE) {
        Some((cut, _)) => Some(format!("{}...", &line[..cut])),
        None => Some(line.to_string()),
    }
}

/// How much work a session did: turns, steps, tool calls, and the wall time
/// spent in the model and in tools.
///
/// **`step/end` counts a step, not `assistant/message`.** The step lifecycle is
/// what the loop guarantees exactly one of per entered step, whichever way that
/// step ended. Counting assembled messages instead would undercount a step an
/// interrupt cut short and overcount one whose message said nothing.
///
/// **Time is the journal's, not the reader's.** Every figure is a difference
/// between two `time` fields on the log, so a replay of the same journal
/// reports the same milliseconds. A clock read while folding would make the
/// value drift on every read and the checkpoint a lie.
#[derive(Debug, Default)]
pub struct Stats;

impl Projection for Stats {
    fn key(&self) -> &str {
        STATS
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn init(&self) -> Value {
        json!({
            "turns": 0,
            "steps": 0,
            "tool_calls": 0,
            "model_ms": 0,
            "tool_ms": 0,
            // The boundaries the totals accrue from. They are part of the
            // state and not of the view, because a reader wants the totals and
            // a checkpoint needs everything the next event might use.
            "last_turn": Value::Null,
            "open_step": Value::Null,
            "pending_calls": json!({}),
        })
    }

    fn apply(&self, mut state: Value, event: &SessionEvent) -> Value {
        match event.ty.as_str() {
            "step/start" => {
                state["open_step"] = json!({
                    "turn": number(event, "turn"),
                    "step": number(event, "step"),
                    "time": event.time,
                });
            }
            "assistant/message" => {
                // Model time is the open step's start to its assembled answer.
                // A step that assembled none - one an interrupt or a provider
                // failure ended - contributes no time, which is the honest
                // answer: nothing was measured.
                if let Some(open) = state["open_step"].as_object().cloned() {
                    if open.get("turn").and_then(Value::as_u64) == number(event, "turn")
                        && open.get("step").and_then(Value::as_u64) == number(event, "step")
                    {
                        let started = open.get("time").and_then(Value::as_u64).unwrap_or(0);
                        add(&mut state, "model_ms", event.time.saturating_sub(started));
                        state["open_step"] = Value::Null;
                    }
                }
            }
            "tool/call" => {
                if let Some(id) = text(event, "id") {
                    state["pending_calls"][id] = json!(event.time);
                }
            }
            "tool/result" => {
                let dispatched = text(event, "call_id")
                    .and_then(|id| state["pending_calls"].get(&id).cloned().map(|at| (id, at)));
                // A result naming a call this fold never saw dispatched is not
                // timed rather than timed from zero: an unmatched pair is a
                // gap in the record, and inventing a duration for it would put
                // the session's whole age into `tool_ms`.
                if let Some((id, at)) = dispatched {
                    let at = at.as_u64().unwrap_or(event.time);
                    add(&mut state, "tool_ms", event.time.saturating_sub(at));
                    add(&mut state, "tool_calls", 1);
                    if let Some(pending) = state["pending_calls"].as_object_mut() {
                        pending.remove(&id);
                    }
                }
            }
            "step/end" => {
                let turn = number(event, "turn");
                if state["last_turn"].as_u64() != turn {
                    add(&mut state, "turns", 1);
                }
                add(&mut state, "steps", 1);
                state["last_turn"] = turn.map_or(Value::Null, |turn| json!(turn));
                state["open_step"] = Value::Null;
            }
            "turn/end" => {
                // A call whose result never landed belongs to a turn that
                // failed or was interrupted. Results land inside their turn,
                // so the leftovers are dropped rather than kept for ever:
                // unbounded state is what makes a persisted checkpoint stop
                // being a shortcut.
                state["pending_calls"] = json!({});
            }
            _ => {}
        }
        state
    }

    fn view(&self, state: &Value) -> Value {
        json!({
            "turns": state["turns"],
            "steps": state["steps"],
            "tool_calls": state["tool_calls"],
            "model_ms": state["model_ms"],
            "tool_ms": state["tool_ms"],
        })
    }
}

fn number(event: &SessionEvent, key: &str) -> Option<u64> {
    event.data.get(key).and_then(Value::as_u64)
}

fn text(event: &SessionEvent, key: &str) -> Option<String> {
    event
        .data
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn add(state: &mut Value, key: &str, by: u64) {
    let now = state[key].as_u64().unwrap_or(0);
    state[key] = json!(now + by);
}
