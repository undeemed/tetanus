//! The questions worth having an answer for, rather than a filter for.
//!
//! Everything here could be written by a caller with [`crate::Journal::select`]
//! and a fold. They are here because each is a question three surfaces would
//! otherwise each answer slightly differently - in particular the pairing of a
//! `tool/result` to its `tool/call`, which contract section 4.3.1 says is by
//! `call_id` and never by arrival order, and which is exactly the rule an
//! ad-hoc fold gets wrong under parallel tool calls.

use std::collections::BTreeMap;

use tetanus_protocol::types::{KnownEvent, StopReason, Usage};

use crate::filter::Bound;
use crate::journal::{Journal, Located};

/// One tool call and the result that answered it, if one did.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRecord {
    /// The `tool/call.id`. What pairs the two events.
    pub call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    /// Where the call was made.
    pub turn: Option<u64>,
    pub step: Option<u32>,
    pub call_seq: u64,
    /// Seq of the `tool/result`, absent on a call the log has no answer for -
    /// a turn a crash cut short, or one still running.
    pub result_seq: Option<u64>,
    /// The result's outcome. `None` means unanswered, which is not failure.
    pub ok: Option<bool>,
    /// The result's content, absent for the same reason `result_seq` is.
    pub output: Option<String>,
}

impl ToolCallRecord {
    /// True only for a call that was answered, and answered badly. A call with
    /// no answer is not a failure: nobody said so.
    pub fn failed(&self) -> bool {
        self.ok == Some(false)
    }
}

/// What a range of turns cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnCost {
    pub usage: Usage,
    /// How many turns contributed. A caller comparing two ranges needs it: a
    /// larger total over more turns is not a more expensive turn.
    pub turns: u64,
    /// How many priced assistant messages were summed.
    pub messages: u64,
    /// Assistant messages in range whose provider reported no usage.
    ///
    /// The reason this is a field and not a footnote: a total is only a total
    /// if nothing was silently left out of it, and a build with an unpriced
    /// adapter would otherwise report a confident, wrong, smaller number.
    pub unpriced: u64,
}

impl TurnCost {
    /// Whether every message in range was priced. A surface renders `1,204` or
    /// `at least 1,204` off this.
    pub fn complete(&self) -> bool {
        self.unpriced == 0
    }

    pub fn total_tokens(&self) -> u64 {
        self.usage.prompt_tokens + self.usage.completion_tokens
    }
}

/// One turn, summarised from its own events.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnRow {
    pub turn: u64,
    /// Steps the log actually recorded, counted rather than read off
    /// `turn/end`: a turn a crash cut short has steps and no `turn/end`.
    pub steps: u32,
    /// Absent on a turn the log never closed.
    pub stop_reason: Option<StopReason>,
    pub tool_calls: u64,
    pub tool_failures: u64,
    pub cost: TurnCost,
    pub first_seq: u64,
    pub last_seq: u64,
    /// Wall clock between the turn's first and last event, in milliseconds.
    pub duration_ms: u64,
}

impl Journal {
    /// Every tool call in this session, in the order they were made, each
    /// paired with its result by `call_id`.
    ///
    /// A result naming a `call_id` no call in this log used is dropped rather
    /// than reported as a call: this returns *calls*, and inventing one from a
    /// stray result would put an event in the answer that never happened.
    pub fn tool_calls(&self) -> Vec<ToolCallRecord> {
        let mut records: Vec<ToolCallRecord> = Vec::new();
        let mut by_id: BTreeMap<String, usize> = BTreeMap::new();

        for located in self.events() {
            match located.event.parse() {
                Some(KnownEvent::ToolCall {
                    id,
                    name,
                    arguments,
                }) => {
                    // A duplicate id shadows the earlier call for pairing
                    // purposes, because a result can only mean the most recent
                    // one. Both calls stay in the answer: both happened.
                    by_id.insert(id.clone(), records.len());
                    records.push(ToolCallRecord {
                        call_id: id,
                        name,
                        arguments,
                        turn: located.turn,
                        step: located.step,
                        call_seq: located.seq(),
                        result_seq: None,
                        ok: None,
                        output: None,
                    });
                }
                Some(KnownEvent::ToolResult {
                    call_id,
                    ok,
                    content,
                    ..
                }) => {
                    if let Some(record) = by_id.get(&call_id).and_then(|at| records.get_mut(*at)) {
                        record.result_seq = Some(located.seq());
                        record.ok = Some(ok);
                        record.output = Some(content);
                    }
                }
                _ => {}
            }
        }

        records
    }

    /// Every turn in which the named tool was called and answered with a
    /// failure, ascending and without repeats.
    ///
    /// Two calls of the same tool failing in one turn name that turn once: the
    /// question is which turns went wrong, and answering `[3, 3]` would make a
    /// caller deduplicate a list this crate had already grouped.
    pub fn turns_failing(&self, tool: &str) -> Vec<u64> {
        let mut turns: Vec<u64> = self
            .tool_calls()
            .into_iter()
            .filter(|record| record.name == tool && record.failed())
            .filter_map(|record| record.turn)
            .collect();
        turns.sort_unstable();
        turns.dedup();
        turns
    }

    /// What a range of turns cost, summed from the usage each assistant
    /// message reported.
    ///
    /// Summed from the journal rather than from a running counter because the
    /// journal is the only source that survives a restart, and a resumed
    /// session's cost is the whole session's cost.
    pub fn cost(&self, turns: Bound<u64>) -> TurnCost {
        let mut cost = TurnCost::default();
        let mut counted: Vec<u64> = Vec::new();

        for located in self.events() {
            let Some(turn) = located.turn else { continue };
            if !turns.contains(turn) {
                continue;
            }
            if !counted.contains(&turn) {
                counted.push(turn);
            }
            let Some(KnownEvent::AssistantMessage { usage, .. }) = located.event.parse() else {
                continue;
            };
            match usage {
                Some(usage) => {
                    cost.messages += 1;
                    cost.usage.prompt_tokens += usage.prompt_tokens;
                    cost.usage.completion_tokens += usage.completion_tokens;
                }
                None => cost.unpriced += 1,
            }
        }

        cost.turns = counted.len() as u64;
        cost
    }

    /// One row per turn the log recorded, ascending.
    pub fn turns(&self) -> Vec<TurnRow> {
        let mut rows: BTreeMap<u64, TurnRow> = BTreeMap::new();

        for located in self.events() {
            let Some(turn) = located.turn else { continue };
            let row = rows.entry(turn).or_insert_with(|| TurnRow {
                turn,
                steps: 0,
                stop_reason: None,
                tool_calls: 0,
                tool_failures: 0,
                cost: TurnCost {
                    turns: 1,
                    ..TurnCost::default()
                },
                first_seq: located.seq(),
                last_seq: located.seq(),
                duration_ms: 0,
            });
            row.last_seq = located.seq();
            fold(row, located);
        }

        for row in rows.values_mut() {
            row.duration_ms = span(self, row);
        }
        rows.into_values().collect()
    }
}

fn fold(row: &mut TurnRow, located: &Located) {
    match located.event.parse() {
        Some(KnownEvent::StepEnd { .. }) => row.steps += 1,
        Some(KnownEvent::TurnEnd { stop_reason, .. }) => row.stop_reason = Some(stop_reason),
        Some(KnownEvent::ToolCall { .. }) => row.tool_calls += 1,
        Some(KnownEvent::ToolResult { ok: false, .. }) => row.tool_failures += 1,
        Some(KnownEvent::AssistantMessage { usage, .. }) => match usage {
            Some(usage) => {
                row.cost.messages += 1;
                row.cost.usage.prompt_tokens += usage.prompt_tokens;
                row.cost.usage.completion_tokens += usage.completion_tokens;
            }
            None => row.cost.unpriced += 1,
        },
        _ => {}
    }
}

/// Wall clock across a turn, from the times its own events carry.
fn span(journal: &Journal, row: &TurnRow) -> u64 {
    let times: Vec<u64> = journal
        .events()
        .iter()
        .filter(|event| event.turn == Some(row.turn))
        .map(Located::time)
        .collect();
    match (times.iter().min(), times.iter().max()) {
        (Some(first), Some(last)) => last.saturating_sub(*first),
        _ => 0,
    }
}
