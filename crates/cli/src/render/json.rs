//! Contract output: the call's own result types, one JSON object per line.
//!
//! Everything else in this directory answers "what should a person read". This
//! module answers the other question, and the two must not be confused: the
//! shapes here are `tetanus-protocol`'s, fixed by the interface contract
//! section 4.7, and this lane may not add a field to one or rename one to read
//! better. A script reads lines until the stream ends and treats the last one
//! as the answer, whichever subcommand it ran.
//!
//! So there is no theme here, no width, and no glyph. A JSON line is the same
//! bytes at a terminal and in a pipe.

use std::io::{self, Write};

use serde::Serialize;
use tetanus_protocol::types::{KnownEvent, SessionEvent, StopReason, TurnSummary, Usage};
use tetanus_ui::Ui;

/// Write one value as one line.
pub fn line<W: Write, T: Serialize>(ui: &mut Ui<W>, value: &T) -> io::Result<()> {
    ui.line(&serde_json::to_string(value).map_err(io::Error::other)?)
}

/// The summary of a turn, read back off the journal the turn wrote.
///
/// `duration_ms` and `usage` are optional on the boundary because a build may
/// not measure them. This one does: both are in the journal, so reporting
/// `None` would be a build understating what it knows. They are derived here
/// rather than carried out of the engine because the engine's own outcome type
/// has no room for them yet - when `agent.prompt` serves this summary itself,
/// this function is what it replaces.
pub fn summary(
    events: &[SessionEvent],
    turn: u64,
    steps: u32,
    stop_reason: StopReason,
    stop_veto: Option<String>,
    content: String,
) -> TurnSummary {
    let mut spent: Option<Usage> = None;
    for event in events {
        if let Some(KnownEvent::AssistantMessage {
            usage: Some(step), ..
        }) = event.parse()
        {
            // Each step is billed for the whole prompt it resent, so a turn
            // costs the sum of its requests and not the last one.
            let total = spent.get_or_insert_with(Usage::default);
            total.prompt_tokens += step.prompt_tokens;
            total.completion_tokens += step.completion_tokens;
        }
    }
    let span = |first: &SessionEvent, last: &SessionEvent| last.time.saturating_sub(first.time);
    TurnSummary {
        turn,
        steps,
        stop_reason,
        stop_veto,
        content,
        duration_ms: events.first().zip(events.last()).map(|(a, b)| span(a, b)),
        usage: spent,
    }
}

/// Test Design Specification: contract output.
///
/// Features tested: the arithmetic of a summary derived from a journal - what
/// a turn spent, and how long it took.
///
/// Features NOT tested here: the JSON shapes themselves (owned by
/// `tetanus-protocol`, and covered by `crates/protocol/tests/wire.rs`) and
/// which subcommand prints what (covered end to end by TC-CLI-JSON-1..4).
///
/// Environmental needs: none.
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(seq: u64, time: u64, ty: &str, data: serde_json::Value) -> SessionEvent {
        SessionEvent {
            ty: ty.into(),
            seq,
            time,
            data,
            source_event_seqs: None,
        }
    }

    fn billed(prompt: u64, completion: u64) -> serde_json::Value {
        json!({
            "content": "on it",
            "usage": { "prompt_tokens": prompt, "completion_tokens": completion }
        })
    }

    fn summarised(events: &[SessionEvent]) -> TurnSummary {
        summary(events, 1, 2, StopReason::Natural, None, "on it".into())
    }

    /// TC-CLI-JSON-5: a summary derived from a two-step journal.
    /// Expected: the tokens of every step added up, and the wall clock from
    /// the first event to the last. Each step is billed for the whole prompt
    /// it resent, so a turn costs the sum of its requests; reporting the last
    /// step alone would understate a long turn by most of its cost.
    #[test]
    fn a_summary_adds_up_what_every_step_spent() {
        let spent = summarised(&[
            event(0, 1_000, "turn/start", json!({ "turn": 1 })),
            event(1, 1_100, "assistant/message", billed(21, 5)),
            event(2, 4_500, "assistant/message", billed(29, 5)),
            event(3, 4_600, "turn/end", json!({ "turn": 1 })),
        ]);

        assert_eq!(spent.usage.expect("usage").prompt_tokens, 50);
        assert_eq!(spent.usage.expect("usage").completion_tokens, 10);
        assert_eq!(spent.duration_ms, Some(3_600));
        assert_eq!(spent.stop_reason, StopReason::Natural);
    }

    /// TC-CLI-JSON-6: a journal whose messages carry no usage.
    /// Expected: `None`, not zero. The contract reserves `None` for "this
    /// build did not measure it", and a turn reported as having spent nothing
    /// is a different claim from one that was never metered.
    #[test]
    fn an_unmetered_turn_reports_no_usage_rather_than_none_spent() {
        let spent = summarised(&[
            event(0, 1_000, "turn/start", json!({ "turn": 1 })),
            event(1, 1_100, "assistant/message", json!({ "content": "on it" })),
        ]);

        assert_eq!(spent.usage, None);
        assert_eq!(spent.duration_ms, Some(100));
    }
}
