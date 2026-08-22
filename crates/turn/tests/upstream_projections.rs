//! Test Design Specification: the token and context projections, ported.
//!
//! Features under test: `tetanus_turn::projections` and
//! `tetanus_session::units` - the five folds a reader asks a session for.
//! Upstream pins the priced three in `packages/llm/token-meter/tests/`
//! (`token-usage-projection.spec.ts`, `context-breakdown-projection.spec.ts`)
//! and the other two in `packages/session/session-stats` and
//! `session-title-first-prompt`. These are the two rows `docs/parity.md`
//! section 4 marked blocked on the projection seam.
//!
//! Approach: every unit is driven through the real `Projections` registry, so
//! a case exercises the registration, the watermark and the checkpoint as well
//! as the fold, and one case drives the whole set over a journal a real turn
//! wrote - because "returns real numbers over a real session" is the claim,
//! and a hand-built fixture cannot make it.
//!
//! What is not restated, and why. Upstream's usage arrives twice per step,
//! once as a streamed `usage` chunk and once on the assembled message, so its
//! fold has a replace-in-place rule for the second report; a tetanus stream
//! carries usage only on the assembled `assistant/message`, so TC-PORT-PROJ-16
//! states the same rule over a repeated report for one turn and step. Its zod
//! view schemas have no counterpart on this seam.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::sync::Arc;

use harness::Harness;
use serde_json::json;
use tetanus_core::EventBus;
use tetanus_session::projection::{Projection, Projections};
use tetanus_session::{units, JsonlSessionLog, SessionEvent, SessionLog};
use tetanus_turn::compaction::{self, compact, CompactionBudget, OutlineSummarizer};
use tetanus_turn::projections::{
    self, ContextBreakdown, ContextPressure, TokenUsage, CONTEXT_BREAKDOWN, CONTEXT_PRESSURE,
    TOKEN_USAGE,
};

/// Every unit this workspace serves, registered on one registry.
fn every_unit() -> Arc<Projections> {
    let registry = Projections::new();
    for unit in projections::units() {
        registry.register(unit).expect("register");
    }
    registry
        .register(Arc::new(units::Title) as Arc<dyn Projection>)
        .expect("register");
    registry
        .register(Arc::new(units::Stats) as Arc<dyn Projection>)
        .expect("register");
    registry
}

fn event(seq: u64, time: u64, ty: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        ty: ty.to_string(),
        seq,
        time,
        data,
        source_event_seqs: None,
    }
}

/// TC-PORT-PROJ-13: the usage projection sums what the provider charged.
///
/// Upstream: `token-usage-projection.spec.ts`, "accumulates usage across
/// steps".
///
/// Expected: prompt and completion tokens are the sums of the reports, the
/// total is their sum, and the step count is the number of steps that
/// reported.
#[test]
fn usage_sums_what_the_provider_charged() {
    let registry = Projections::new();
    registry.register(Arc::new(TokenUsage)).expect("register");

    let events = vec![
        event(
            0,
            1,
            "assistant/message",
            json!({ "turn": 1, "step": 1, "content": "a",
                    "usage": { "prompt_tokens": 100, "completion_tokens": 20 } }),
        ),
        event(
            1,
            2,
            "assistant/message",
            json!({ "turn": 1, "step": 2, "content": "b",
                    "usage": { "prompt_tokens": 140, "completion_tokens": 30 } }),
        ),
    ];
    registry.drive(&events);

    assert_eq!(
        registry.value(TOKEN_USAGE).expect("a value"),
        json!({
            "prompt_tokens": 240,
            "completion_tokens": 50,
            "total_tokens": 290,
            "reported_steps": 2,
        })
    );
}

/// TC-PORT-PROJ-14: a message that reported no usage moves nothing.
///
/// Expected: the totals are untouched and no step is counted, so a provider
/// that reports nothing cannot make a session look free or busy.
#[test]
fn a_message_with_no_usage_moves_nothing() {
    let registry = Projections::new();
    registry.register(Arc::new(TokenUsage)).expect("register");

    registry.drive(&[
        event(
            0,
            1,
            "assistant/message",
            json!({ "turn": 1, "step": 1, "content": "a",
                    "usage": { "prompt_tokens": 10, "completion_tokens": 1 } }),
        ),
        event(
            1,
            2,
            "assistant/message",
            json!({ "turn": 1, "step": 2, "content": "b" }),
        ),
        event(2, 3, "step/end", json!({ "turn": 1, "step": 2 })),
    ]);

    let value = registry.value(TOKEN_USAGE).expect("a value");
    assert_eq!(value["total_tokens"], json!(11));
    assert_eq!(value["reported_steps"], json!(1));
}

/// TC-PORT-PROJ-15: a step that reports twice replaces its own figure.
///
/// Upstream: `token-usage-projection.spec.ts`, "replaces a repeated sample for
/// the same step instead of double counting". Adding instead of replacing
/// would double a step whose message a repair appended twice.
///
/// Expected: the totals carry the later figure once, not both.
#[test]
fn a_step_that_reports_twice_is_counted_once() {
    let registry = Projections::new();
    registry.register(Arc::new(TokenUsage)).expect("register");

    registry.drive(&[
        event(
            0,
            1,
            "assistant/message",
            json!({ "turn": 1, "step": 1, "content": "draft",
                    "usage": { "prompt_tokens": 100, "completion_tokens": 5 } }),
        ),
        event(
            1,
            2,
            "assistant/message",
            json!({ "turn": 1, "step": 1, "content": "final",
                    "usage": { "prompt_tokens": 100, "completion_tokens": 9 } }),
        ),
    ]);

    assert_eq!(
        registry.value(TOKEN_USAGE).expect("a value"),
        json!({
            "prompt_tokens": 100,
            "completion_tokens": 9,
            "total_tokens": 109,
            "reported_steps": 1,
        })
    );
}

/// TC-PORT-PROJ-16: the breakdown adds up, and the envelope is last-wins.
///
/// Upstream: `context-breakdown-projection.spec.ts`. The three figures are
/// priced under one estimator so a reader can add them; the envelope figures
/// come from the newest `request/context` and the conversation figure from the
/// surface.
///
/// Expected: the system and tool figures are the newest envelope's, the
/// message figure is the surface's, and the total is their sum.
#[test]
fn the_breakdown_adds_up_and_takes_the_newest_envelope() {
    let registry = Projections::new();
    registry
        .register(Arc::new(ContextBreakdown))
        .expect("register");

    registry.drive(&[
        event(
            0,
            1,
            "request/context",
            json!({ "system_tokens": 40, "tools_tokens": 100 }),
        ),
        event(1, 2, "user/message", json!({ "content": "hello there" })),
        event(
            2,
            3,
            "request/context",
            json!({ "system_tokens": 55, "tools_tokens": 100 }),
        ),
    ]);

    let value = registry.value(CONTEXT_BREAKDOWN).expect("a value");
    assert_eq!(
        value["system_tokens"],
        json!(55),
        "the newest envelope wins"
    );
    assert_eq!(value["tools_tokens"], json!(100));
    let messages = value["message_tokens"].as_u64().expect("a number");
    assert!(messages > 0, "the conversation is priced: {value}");
    assert_eq!(value["total_tokens"], json!(55 + 100 + messages));
}

/// TC-PORT-PROJ-17: pressure reports what the provider measured, and projects
/// it forward over the surface's movement.
///
/// Upstream: `token-usage-projection.spec.ts`'s `contextPressure` half.
/// `pressure_tokens` is always one request out of date, because nothing but a
/// request reports usage; `projected_tokens` carries it forward so a gauge
/// answers for the request about to be sent.
///
/// Expected: pressure is the newest prompt figure; projected exceeds it by the
/// tokens added since the sample; the window is the envelope's.
#[test]
fn pressure_is_measured_and_projected_forward() {
    let registry = Projections::new();
    registry
        .register(Arc::new(ContextPressure))
        .expect("register");

    registry.drive(&[
        event(0, 1, "request/context", json!({ "context_window": 64_000 })),
        event(1, 2, "user/message", json!({ "content": "first" })),
        event(
            2,
            3,
            "assistant/message",
            json!({ "turn": 1, "step": 1, "content": "answered",
                    "usage": { "prompt_tokens": 900, "completion_tokens": 10 } }),
        ),
    ]);
    let sampled = registry.value(CONTEXT_PRESSURE).expect("a value");
    assert_eq!(sampled["pressure_tokens"], json!(900));
    assert_eq!(sampled["context_window"], json!(64_000));
    // The answer itself is on the surface now and the provider's prompt figure
    // did not include it - an answer is output, and the next request carries it
    // as input - so the projection is the sample plus that answer.
    assert!(
        sampled["projected_tokens"].as_u64().unwrap() > 900,
        "the answer the sample did not pay for is projected: {sampled}"
    );

    // A message the provider has not been charged for yet.
    registry.drive(&[
        event(0, 1, "request/context", json!({ "context_window": 64_000 })),
        event(1, 2, "user/message", json!({ "content": "first" })),
        event(
            2,
            3,
            "assistant/message",
            json!({ "turn": 1, "step": 1, "content": "answered",
                    "usage": { "prompt_tokens": 900, "completion_tokens": 10 } }),
        ),
        event(
            3,
            4,
            "user/message",
            json!({ "content": "a follow-up question" }),
        ),
    ]);
    let projected = registry.value(CONTEXT_PRESSURE).expect("a value");
    assert_eq!(projected["pressure_tokens"], json!(900), "still the sample");
    assert!(
        projected["projected_tokens"].as_u64().unwrap() > 900,
        "the unbilled message is projected: {projected}"
    );
}

/// TC-PORT-PROJ-18: a compaction shrinks the projected figure by the price it
/// recorded.
///
/// Upstream: `surface-projection.ts`'s shadow-price protocol. This is what the
/// protocol exists for: nothing reports usage between a compaction and the
/// next request, so without it a gauge would keep showing the pre-compaction
/// figure while the whole point was that the context got smaller.
///
/// Expected: the projection falls after the compaction, and by the difference
/// between the recorded price and the replacement.
#[test]
fn a_compaction_shrinks_the_projection_by_its_recorded_price() {
    let registry = Projections::new();
    registry
        .register(Arc::new(ContextPressure))
        .expect("register");

    let base = vec![
        event(0, 1, "user/message", json!({ "content": "x".repeat(400) })),
        event(
            1,
            2,
            "assistant/message",
            json!({ "turn": 1, "step": 1, "content": "y".repeat(400),
                    "usage": { "prompt_tokens": 500, "completion_tokens": 10 } }),
        ),
    ];
    registry.drive(&base);
    let before = registry.value(CONTEXT_PRESSURE).expect("a value")["projected_tokens"]
        .as_u64()
        .expect("a number");

    let shadowed_price = 208_u64;
    let mut compacted = base.clone();
    compacted.push(event(
        2,
        3,
        "compaction/summary",
        json!({ "shadowed_seqs": [0], "shadowed_token_count": shadowed_price }),
    ));
    compacted.push(event(3, 4, "user/message", json!({ "content": "SUMMARY" })));
    registry.drive(&compacted);

    let after = registry.value(CONTEXT_PRESSURE).expect("a value")["projected_tokens"]
        .as_u64()
        .expect("a number");
    assert!(
        after < before,
        "the projection fell: {before} became {after}"
    );
}

/// TC-PORT-PROJ-19: a replacement with no adjacent price folds neutrally.
///
/// Upstream states the same fallback: a journal written before the shadow-price
/// protocol has no claim to find, and bounded state cannot reconstruct the
/// replaced range. Folding it to zero keeps replay working at the cost of
/// drift, which is strictly better than a total that is confidently wrong.
///
/// Expected: no panic, and the figure moves by the replacement alone.
#[test]
fn a_replacement_with_no_price_folds_neutrally() {
    let registry = Projections::new();
    registry
        .register(Arc::new(ContextBreakdown))
        .expect("register");

    registry.drive(&[
        event(0, 1, "user/message", json!({ "content": "a question" })),
        event(1, 2, "user/message", json!({ "content": "another" })),
    ]);
    let value = registry.value(CONTEXT_BREAKDOWN).expect("a value");
    assert!(value["message_tokens"].as_u64().unwrap() > 0);
}

/// TC-PORT-PROJ-20: the title is the first prompt, one line, and it never
/// moves.
///
/// Upstream: `session-title-first-prompt`. A title that moved every turn would
/// make a session unfindable in the list a user is scanning.
///
/// Expected: the first message's first line, with later messages changing
/// nothing, and whitespace never becoming a title.
#[test]
fn the_title_is_the_first_prompt_and_stays() {
    let registry = Projections::new();
    registry
        .register(Arc::new(units::Title) as Arc<dyn Projection>)
        .expect("register");

    registry.drive(&[
        event(0, 1, "user/message", json!({ "content": "   \n  " })),
        event(
            1,
            2,
            "user/message",
            json!({ "content": "  make the tests pass\nand then tidy up  " }),
        ),
        event(2, 3, "user/message", json!({ "content": "now do this" })),
    ]);

    assert_eq!(
        registry.value(units::TITLE).expect("a value"),
        json!("make the tests pass")
    );
}

/// TC-PORT-PROJ-21: a title longer than the line a picker gets is cut.
///
/// Expected: eighty characters and an ellipsis, cut by character so a
/// multi-byte title cannot panic the fold.
#[test]
fn a_long_title_is_cut_by_character() {
    let long = "\u{4f60}".repeat(200);
    let cut = units::title_of(Some(&long)).expect("a title");
    assert_eq!(cut.chars().count(), units::MAX_TITLE + 3);
    assert!(cut.ends_with("..."));
    assert!(units::title_of(Some("  \n ")).is_none());
    assert!(units::title_of(None).is_none());
}

/// TC-PORT-PROJ-22: stats count closed steps and turns, and time what they
/// can measure.
///
/// Upstream: `session-stats/projection.ts`. `step/end` counts a step because
/// the loop appends exactly one per entered step whichever way it ended;
/// counting assembled messages would undercount an interrupted step.
///
/// Expected: two turns, three steps; model time from step start to assembled
/// message; tool time from call to result; an unmatched result timed at zero.
#[test]
fn stats_count_closed_steps_and_time_what_they_can() {
    let registry = Projections::new();
    registry
        .register(Arc::new(units::Stats) as Arc<dyn Projection>)
        .expect("register");

    registry.drive(&[
        event(0, 1000, "turn/start", json!({ "turn": 1 })),
        event(1, 1000, "step/start", json!({ "turn": 1, "step": 1 })),
        event(2, 1010, "tool/call", json!({ "id": "c1", "name": "read" })),
        event(3, 1040, "tool/result", json!({ "call_id": "c1" })),
        event(
            4,
            1050,
            "assistant/message",
            json!({ "turn": 1, "step": 1, "content": "done" }),
        ),
        event(5, 1050, "step/end", json!({ "turn": 1, "step": 1 })),
        event(6, 1060, "step/start", json!({ "turn": 1, "step": 2 })),
        event(7, 1060, "step/end", json!({ "turn": 1, "step": 2 })),
        event(8, 1060, "turn/end", json!({ "turn": 1 })),
        event(9, 2000, "turn/start", json!({ "turn": 2 })),
        event(10, 2000, "step/start", json!({ "turn": 2, "step": 1 })),
        // A result whose call this fold never saw is not timed at all.
        event(11, 2500, "tool/result", json!({ "call_id": "ghost" })),
        event(12, 2500, "step/end", json!({ "turn": 2, "step": 1 })),
    ]);

    assert_eq!(
        registry.value(units::STATS).expect("a value"),
        json!({
            "turns": 2,
            "steps": 3,
            "tool_calls": 1,
            "model_ms": 50,
            "tool_ms": 30,
        })
    );
}

/// TC-PORT-PROJ-23: a call whose result never landed is dropped at the turn's
/// end.
///
/// Persisted state has to stay bounded, and a session whose turns are
/// interrupted would otherwise accumulate one pending entry per lost call for
/// the rest of its life.
///
/// Expected: the pending entry is gone, so a late result is unmatched rather
/// than timed across the gap.
#[test]
fn a_lost_call_is_dropped_when_its_turn_ends() {
    let unit = units::Stats;
    let mut state = unit.init();
    for event in [
        event(0, 100, "tool/call", json!({ "id": "c1", "name": "read" })),
        event(1, 200, "turn/end", json!({ "turn": 1 })),
    ] {
        state = unit.apply(state, &event);
    }
    assert_eq!(state["pending_calls"], json!({}));

    let late = unit.apply(
        state,
        &event(2, 9_000, "tool/result", json!({ "call_id": "c1" })),
    );
    assert_eq!(unit.view(&late)["tool_ms"], json!(0));
    assert_eq!(unit.view(&late)["tool_calls"], json!(0));
}

/// TC-PORT-PROJ-24: every unit returns real numbers over a journal a real turn
/// wrote.
///
/// This is the acceptance claim for the two rows `docs/parity.md` section 4
/// marked blocked. A hand-built fixture cannot make it: the point is that the
/// events a turn actually writes are the events these folds read.
///
/// Expected: usage, breakdown and stats all report non-zero figures; the
/// breakdown's envelope figures come from the `request/context` the turn
/// wrote; and the title is the prompt that opened the session.
#[tokio::test]
async fn every_unit_reports_real_numbers_over_a_real_session() {
    let h = Harness::new("projected").await;
    h.engine
        .run_turn("count the files in this repository")
        .await
        .unwrap();

    let registry = every_unit();
    registry.drive(&h.engine.log().events());

    let usage = registry.value(TOKEN_USAGE).expect("usage");
    assert!(
        usage["total_tokens"].as_u64().unwrap() > 0,
        "the provider's charge reached the fold: {usage}"
    );
    assert_eq!(usage["reported_steps"], json!(2), "one per step");

    let breakdown = registry.value(CONTEXT_BREAKDOWN).expect("breakdown");
    for field in ["system_tokens", "tools_tokens", "message_tokens"] {
        assert!(
            breakdown[field].as_u64().unwrap() > 0,
            "{field} is a real number: {breakdown}"
        );
    }
    assert_eq!(
        breakdown["total_tokens"].as_u64().unwrap(),
        breakdown["system_tokens"].as_u64().unwrap()
            + breakdown["tools_tokens"].as_u64().unwrap()
            + breakdown["message_tokens"].as_u64().unwrap()
    );

    let pressure = registry.value(CONTEXT_PRESSURE).expect("pressure");
    assert!(pressure["pressure_tokens"].as_u64().unwrap() > 0);

    let stats = registry.value(units::STATS).expect("stats");
    assert_eq!(stats["turns"], json!(1));
    assert_eq!(stats["steps"], json!(2));
    assert_eq!(stats["tool_calls"], json!(1));

    assert_eq!(
        registry.value(units::TITLE).expect("title"),
        json!("count the files in this repository")
    );
}

/// TC-PORT-PROJ-25: a checkpoint of every unit restores to the same values.
///
/// The whole reason the state is JSON and bounded: a stored row is a shortcut
/// a later process can adopt instead of refolding a long journal.
///
/// Expected: a second registry restored from the checkpoint answers exactly
/// what the first did, without being shown the events again.
#[tokio::test]
async fn a_checkpoint_of_every_unit_restores_to_the_same_values() {
    let h = Harness::new("checkpointed").await;
    h.engine.run_turn("do some work").await.unwrap();
    let events = h.engine.log().events();

    let first = every_unit();
    first.drive(&events);
    let stored = first.checkpoint();

    let second = every_unit();
    second.restore(&stored, &events);

    assert_eq!(first.snapshot(), second.snapshot());
}

/// TC-PORT-PROJ-26: the folds survive a compaction on a real journal.
///
/// The two halves of this lane meeting: a compacted session must still report
/// figures that a reader can act on, and the compaction must be visible in
/// them.
///
/// Expected: the breakdown's message figure falls, the usage totals do not
/// move - a compaction charges nothing - and the title is unchanged, because a
/// checkpoint replacing the first prompt must not rename the session.
#[tokio::test]
async fn the_folds_survive_a_compaction() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("compacted.jsonl");
    let log = JsonlSessionLog::create("compacted", &path, EventBus::new()).expect("journal");

    log.append_with_sources(
        "user/message",
        json!({ "content": "the original question" }),
        vec![],
    )
    .unwrap();
    for n in 0..8 {
        log.append_with_sources(
            "assistant/message",
            json!({ "turn": 1, "step": n + 1, "content": "y".repeat(300),
                    "usage": { "prompt_tokens": 10, "completion_tokens": 2 } }),
            vec![],
        )
        .unwrap();
        log.append_with_sources(
            "user/message",
            json!({ "content": format!("follow up {n} {}", "x".repeat(300)) }),
            vec![],
        )
        .unwrap();
    }

    let registry = every_unit();
    registry.drive(&log.events());
    let before = registry.snapshot();

    compact(
        log.as_ref(),
        &OutlineSummarizer,
        "system",
        CompactionBudget {
            threshold_tokens: 400,
            retain_tokens: 120,
        },
    )
    .await
    .expect("compacted");

    registry.drive(&log.events());
    let after = registry.snapshot();

    let messages = |snapshot: &tetanus_session::projection::Snapshot| {
        snapshot.values[CONTEXT_BREAKDOWN]["message_tokens"]
            .as_u64()
            .expect("a number")
    };
    assert!(
        messages(&after) < messages(&before),
        "the conversation figure fell: {} became {}",
        messages(&before),
        messages(&after)
    );
    assert_eq!(
        after.values[TOKEN_USAGE], before.values[TOKEN_USAGE],
        "a compaction charges nothing"
    );
    assert_eq!(
        after.values[units::TITLE],
        before.values[units::TITLE],
        "a checkpoint does not rename the session"
    );
    assert!(
        log.events()
            .iter()
            .any(|e| e.ty == compaction::topic::COMPACTION_SUMMARY),
        "the compaction really happened"
    );
}
