//! Folding every matched hook's answer at one point into a single outcome.
//!
//! Several hooks can match the same point, and they can disagree. The fold is
//! deliberately not a vote: it takes the *most restrictive* answer, because a
//! hook exists to stop something and one that is outvoted has not been heard.
//!
//! Four separate rules, each with its own reason:
//!
//! - **Permission is `deny > ask > allow`.** Expressed as an ordering on
//!   [`MergedDecision`], so the fold is `max` and cannot drift.
//! - **Only the winning answer's reasons surface.** A denial explained by the
//!   text of an unrelated `allow` would misdescribe why the call was refused.
//! - **A halt is sticky, and keeps the *first* halting hook's reason.** Later
//!   halts are already implied by the first one.
//! - **Context and warnings accumulate in hook order, empties skipped.** They
//!   are not merged and not joined: what separates them is a decision for
//!   whoever renders them.
//!
//! Parity: upstream `packages/hooks/hook-protocol/src/merge.ts`, pinned by its
//! `merge.spec.ts`.

use crate::types::{HookOutput, MergedDecision, MergedHookOutcome};

/// How reasons are separated when several hooks explain the same answer.
const REASON_SEPARATOR: &str = "\n\n";

/// Fold every matched hook's answer, in hook order, into one outcome.
///
/// An empty list folds to the neutral outcome, which the caller reads as "no
/// hook had anything to say" rather than as an answer.
pub fn merge_hook_outputs(outputs: &[HookOutput]) -> MergedHookOutcome {
    let mut merged = MergedHookOutcome::default();
    // Reasons are kept beside the answer they explain, because which ones
    // surface is not known until every hook has been read.
    let mut reasons: Vec<(MergedDecision, &str)> = Vec::new();

    for output in outputs {
        let decision = output.decision.map_or(MergedDecision::None, |d| d.merged());
        merged.decision = merged.decision.max(decision);

        // An `allow` needs no explanation, so its reason is never collected -
        // not even to be discarded later, which keeps "why is this text here"
        // answerable at the point it is kept.
        if matches!(decision, MergedDecision::Deny | MergedDecision::Ask) {
            if let Some(reason) = non_empty(output.reason.as_deref()) {
                reasons.push((decision, reason));
            }
        }

        if output.proceed == Some(false) && !merged.stop {
            merged.stop = true;
            merged.stop_reason = output.stop_reason.clone();
        }

        if let Some(context) = non_empty(output.additional_context.as_deref()) {
            merged.additional_context.push(context.to_owned());
        }
        if let Some(message) = non_empty(output.system_message.as_deref()) {
            merged.system_messages.push(message.to_owned());
        }
    }

    let winning: Vec<&str> = reasons
        .iter()
        .filter(|(decision, _)| *decision == merged.decision)
        .map(|(_, reason)| *reason)
        .collect();
    if !winning.is_empty() {
        merged.reason = Some(winning.join(REASON_SEPARATOR));
    }

    merged
}

/// A field a hook left empty says nothing, and is treated as absent.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}
