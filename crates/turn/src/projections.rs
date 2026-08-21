//! The three priced projections: what the provider charged, what the next
//! request will occupy, and what the context is made of.
//!
//! They live here rather than beside [`tetanus_session::units`] because each
//! prices something, and pricing is [`crate::tokens`]'s - a listing that wants
//! a title must not have to link a provider adapter, and a gauge that wants a
//! token count must.
//!
//! **The state stays a fixed handful of numbers.** A projection's whole state
//! is checkpointed, so a unit that kept one entry per message would grow a
//! stored row without bound over a session's life. That is why the surface
//! total is a running sum with at most one pending claim rather than a list of
//! priced nodes, and it is the constraint that shapes all three units.
//!
//! **A compaction is priced by the record in front of it.** A replacement
//! shrinks the surface, and a bounded fold cannot reconstruct what it replaced,
//! so the `compaction/summary` or `compaction/prune` record immediately before
//! a replacement states the price of the exact range being replaced. The fold
//! subtracts that and adds the replacement. This is upstream's shadow-price
//! protocol, and [`crate::compaction`] is the producer that must keep it.
//!
//! Parity: upstream `packages/llm/token-meter`'s `usage-projection.ts`,
//! `breakdown-projection.ts` and `surface-projection.ts`, pinned by
//! `token-usage-projection.spec.ts` and `context-breakdown-projection.spec.ts`.
//! Upstream's usage arrives twice per step - once as a streamed `usage` chunk
//! and once on the assembled message - so its fold has a replace-in-place rule
//! for the second report; a tetanus stream carries usage only on the assembled
//! `assistant/message`, so the same rule is stated over repeated reports for
//! one turn and step rather than over two event types.

use serde_json::{json, Value};

use tetanus_session::projection::Projection;
use tetanus_session::SessionEvent;

use crate::compaction::{self, shadow_price};
use crate::log::{derive_messages, topic};
use crate::tokens::{estimate_message, is_surface_event};

/// The key [`TokenUsage`] serves under.
pub const TOKEN_USAGE: &str = "token.usage";
/// The key [`ContextPressure`] serves under.
pub const CONTEXT_PRESSURE: &str = "context.pressure";
/// The key [`ContextBreakdown`] serves under.
pub const CONTEXT_BREAKDOWN: &str = "context.breakdown";

/// What the provider actually charged, summed over the session.
///
/// **Provider-reported, never estimated.** [`crate::tokens`] prices what a
/// request will cost before anyone has answered; this reports what was
/// charged. Mixing the two would give a total that is neither.
///
/// **A step reported twice replaces its own figure.** The one slot for the
/// last sample relies on the log invariant that a step's usage reports are
/// adjacent: once a later step begins, a well-formed journal never reports
/// usage for an earlier one again. Adding instead of replacing would double a
/// step whose message was appended twice by a repair.
///
/// **A report belongs to the step that is open, not to coordinates on the
/// report.** An `assistant/message` carries no turn or step - contract section
/// 4.3.1 gives it `content`, `reasoning`, `tool_calls`, `finish_reason` and
/// `usage` - so the enclosing `step/start` is what identifies it. Reading
/// coordinates that are not there would make every step in a turn look like a
/// repeat of the first one, and a whole turn would be counted once.
#[derive(Debug, Default)]
pub struct TokenUsage;

impl Projection for TokenUsage {
    fn key(&self) -> &str {
        TOKEN_USAGE
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn init(&self) -> Value {
        json!({
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0,
            "reported_steps": 0,
            "open_step": Value::Null,
            "last": Value::Null,
        })
    }

    fn apply(&self, mut state: Value, event: &SessionEvent) -> Value {
        if event.ty == topic::STEP_START {
            state["open_step"] = json!({
                "turn": event.data.get("turn"),
                "step": event.data.get("step"),
            });
            return state;
        }
        if event.ty != topic::ASSISTANT_MESSAGE {
            return state;
        }
        let Some(usage) = event.data.get("usage").filter(|u| !u.is_null()) else {
            return state;
        };
        let prompt = usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let completion = usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        // The open step when there is one; otherwise the report's own
        // coordinates, so a hand-built log that states them is still read the
        // way it reads.
        let coordinates = match state["open_step"].clone() {
            Value::Null => json!({
                "turn": event.data.get("turn"),
                "step": event.data.get("step"),
            }),
            open => open,
        };

        let repeat = state["last"]
            .get("at")
            .is_some_and(|at| *at == coordinates)
            .then(|| {
                (
                    state["last"]["prompt_tokens"].as_u64().unwrap_or(0),
                    state["last"]["completion_tokens"].as_u64().unwrap_or(0),
                )
            });
        let (was_prompt, was_completion) = repeat.unwrap_or((0, 0));
        if repeat.is_none() {
            bump(&mut state, "reported_steps", 1);
        }

        replace(&mut state, "prompt_tokens", was_prompt, prompt);
        replace(&mut state, "completion_tokens", was_completion, completion);
        replace(
            &mut state,
            "total_tokens",
            was_prompt + was_completion,
            prompt + completion,
        );
        state["last"] = json!({
            "at": coordinates,
            "prompt_tokens": prompt,
            "completion_tokens": completion,
        });
        state
    }

    fn view(&self, state: &Value) -> Value {
        json!({
            "prompt_tokens": state["prompt_tokens"],
            "completion_tokens": state["completion_tokens"],
            "total_tokens": state["total_tokens"],
            "reported_steps": state["reported_steps"],
        })
    }
}

/// How full the context is, and how full the next request will be.
///
/// `pressure_tokens` is the prompt side of the newest provider-reported
/// sample: what the last request actually occupied, by the provider's own
/// count. It is the trustworthy number, and it is always one request out of
/// date, because nothing but a request reports usage.
///
/// `projected_tokens` is that sample carried forward by the surface's signed
/// movement since it was taken, so a gauge answers for the request about to be
/// sent rather than the one already answered. It is what makes a compaction
/// visible at all: nothing reports usage between a compaction and the next
/// request, so `pressure_tokens` alone would still show the pre-compaction
/// figure while the whole point was that the context got smaller.
#[derive(Debug, Default)]
pub struct ContextPressure;

impl Projection for ContextPressure {
    fn key(&self) -> &str {
        CONTEXT_PRESSURE
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn init(&self) -> Value {
        json!({
            "surface_tokens": 0,
            "pressure_tokens": Value::Null,
            "sampled_surface_tokens": Value::Null,
            "context_window": Value::Null,
            "claim": Value::Null,
        })
    }

    fn apply(&self, mut state: Value, event: &SessionEvent) -> Value {
        let fold = fold_surface(state["claim"].clone(), event);

        if event.ty == compaction::topic::REQUEST_CONTEXT {
            if let Some(window) = event.data.get("context_window").and_then(Value::as_u64) {
                state["context_window"] = json!(window);
            }
        }

        // The sample is stamped against the surface as it was *before* this
        // event joined it, so an `assistant/message` anchors on the surface
        // its own request actually saw.
        if event.ty == topic::ASSISTANT_MESSAGE {
            if let Some(prompt) = event
                .data
                .get("usage")
                .and_then(|usage| usage.get("prompt_tokens"))
                .and_then(Value::as_u64)
            {
                state["pressure_tokens"] = json!(prompt);
                state["sampled_surface_tokens"] = state["surface_tokens"].clone();
            }
        }

        let surface = state["surface_tokens"].as_i64().unwrap_or(0);
        state["surface_tokens"] = json!((surface + fold.delta).max(0));
        state["claim"] = fold.claim;
        state
    }

    fn view(&self, state: &Value) -> Value {
        let mut view = json!({ "surface_tokens": state["surface_tokens"] });
        if let Some(window) = state["context_window"].as_u64() {
            view["context_window"] = json!(window);
        }
        if let Some(pressure) = state["pressure_tokens"].as_i64() {
            view["pressure_tokens"] = json!(pressure);
            let sampled = state["sampled_surface_tokens"].as_i64().unwrap_or(0);
            let now = state["surface_tokens"].as_i64().unwrap_or(0);
            view["projected_tokens"] = json!((pressure + now - sampled).max(0));
        }
        view
    }
}

/// What the context is made of: the system prompt, the tool catalog, and the
/// conversation, priced under one estimator so the three add up.
///
/// The envelope figures are last-wins from the `request/context` record a step
/// writes before it dispatches; the conversation figure rides the same surface
/// fold [`ContextPressure`] uses, so a compaction shrinks it by exactly the
/// price the compaction recorded.
#[derive(Debug, Default)]
pub struct ContextBreakdown;

impl Projection for ContextBreakdown {
    fn key(&self) -> &str {
        CONTEXT_BREAKDOWN
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn init(&self) -> Value {
        json!({
            "system_tokens": 0,
            "tools_tokens": 0,
            "message_tokens": 0,
            "claim": Value::Null,
        })
    }

    fn apply(&self, mut state: Value, event: &SessionEvent) -> Value {
        let fold = fold_surface(state["claim"].clone(), event);

        if event.ty == compaction::topic::REQUEST_CONTEXT {
            for field in ["system_tokens", "tools_tokens"] {
                if let Some(tokens) = event.data.get(field).and_then(Value::as_u64) {
                    state[field] = json!(tokens);
                }
            }
        }

        let messages = state["message_tokens"].as_i64().unwrap_or(0);
        state["message_tokens"] = json!((messages + fold.delta).max(0));
        state["claim"] = fold.claim;
        state
    }

    fn view(&self, state: &Value) -> Value {
        let system = state["system_tokens"].as_u64().unwrap_or(0);
        let tools = state["tools_tokens"].as_u64().unwrap_or(0);
        let messages = state["message_tokens"].as_u64().unwrap_or(0);
        json!({
            "system_tokens": system,
            "tools_tokens": tools,
            "message_tokens": messages,
            "total_tokens": system + tools + messages,
        })
    }
}

/// Every unit this module serves, for a caller that wants the priced set
/// registered rather than three names it has to remember.
pub fn units() -> Vec<std::sync::Arc<dyn Projection>> {
    vec![
        std::sync::Arc::new(TokenUsage),
        std::sync::Arc::new(ContextPressure),
        std::sync::Arc::new(ContextBreakdown),
    ]
}

/// One event's effect on a running surface total.
struct SurfaceFold {
    /// Signed change in the total; zero for an event off the surface.
    delta: i64,
    /// The shadow price to carry into the next event, if one survives.
    claim: Value,
}

/// Fold one committed event onto a running surface-token total.
///
/// A shadow-price record arms a claim; every other event expires it, and the
/// replacement that follows consumes the claim naming its own range. The
/// producer appends the two adjacently, so a surviving claim always prices the
/// very next event.
///
/// A replacement with no armed claim folds to zero rather than to a guess.
/// Bounded state cannot reconstruct what was replaced, and a journal written
/// before this protocol existed has no claim to find; folding it neutrally
/// keeps replay working at the cost of a total that drifts, which is strictly
/// better than a total that is confidently wrong.
fn fold_surface(claim: Value, event: &SessionEvent) -> SurfaceFold {
    if let Some(price) = shadow_price(event) {
        return SurfaceFold {
            delta: 0,
            claim: json!({ "seqs": price.shadowed_seqs, "tokens": price.shadowed_token_count }),
        };
    }
    if !is_surface_event(event) {
        return SurfaceFold {
            delta: 0,
            claim: Value::Null,
        };
    }
    let tokens = derive_messages(std::slice::from_ref(event))
        .first()
        .map_or(0, estimate_message) as i64;
    let priced = claim
        .get("tokens")
        .and_then(Value::as_i64)
        // A claim is armed only by a record whose next event is its
        // replacement, so an armed claim on a plain append cannot happen from
        // a well-formed producer; folding it as a replacement anyway is what
        // keeps one malformed pair from unbalancing every later total.
        .unwrap_or(0);
    SurfaceFold {
        delta: tokens - priced,
        claim: Value::Null,
    }
}

fn bump(state: &mut Value, key: &str, by: u64) {
    let now = state[key].as_u64().unwrap_or(0);
    state[key] = json!(now + by);
}

fn replace(state: &mut Value, key: &str, was: u64, now: u64) {
    let total = state[key].as_u64().unwrap_or(0);
    state[key] = json!(total.saturating_sub(was) + now);
}
