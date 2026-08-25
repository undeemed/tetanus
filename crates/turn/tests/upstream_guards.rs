//! Test Design Specification: the bounds a deployment sets on a turn.
//!
//! Feature under test: `tetanus_turn::guard` and the two stop reasons it
//! produces - `"timed-out"` and `"repeated"` - which contract section 4.4.2
//! has published since before anything could write them.
//!
//! Upstream's `guard/` packages are the nearest relatives and deliberately
//! weaker: `timeout-policy` bounds one *tool call*, and `repeat-tool-reminder`
//! counts consecutive identical calls and adds a reminder, vetoing nothing.
//! What ports is the detection rule; the action is tetanus's own published
//! one, which is to end the turn and say which guard did it.
//!
//! Approach: the watch is driven directly where the rule is about counting or
//! about a clock, so a case pins the rule without spending a budget in real
//! time; the engine cases run a real turn through the offline harness and read
//! the journal back, because "the journal is balanced" and "the summary is a
//! summary, not an error" are claims about a whole turn.
//!
//! Features NOT tested here: the request-level deadline and idle window, which
//! bound one provider call and have their own suite.
//!
//! Environmental needs: none. No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::time::{Duration, Instant};

use harness::Harness;
use serde_json::json;
use tetanus_turn::events::LlmStream;
use tetanus_turn::guard::{GuardBreach, TurnGuards, TurnWatch};
use tetanus_turn::llm::ModelResponse;
use tetanus_turn::log::topic;
use tetanus_turn::tools::{ToolCall, ToolRegistry};
use tetanus_turn::{StopReason, TurnConfig};

fn call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("call_{name}"),
        name: name.to_string(),
        arguments: args,
    }
}

/// TC-GUARD-1: a turn past its budget is stopped, and says which guard.
///
/// The clock is monotonic and the budget is measured from the turn's start, so
/// a case can place that start in the past rather than sleeping through a
/// budget - which is also why the guard is immune to a wall clock that moves.
///
/// Input: a watch with a one-second budget, started two seconds ago.
/// Expected: `TimedOut`, and no breach at all for a watch with no budget.
#[test]
fn a_turn_past_its_budget_is_stopped() {
    let guards = TurnGuards {
        max_duration: Some(Duration::from_secs(1)),
        ..TurnGuards::default()
    };
    let spent = TurnWatch::started_at(guards, Instant::now() - Duration::from_secs(2));
    assert_eq!(spent.breached(), Some(GuardBreach::TimedOut));

    let unbounded = TurnWatch::started_at(
        TurnGuards::default(),
        Instant::now() - Duration::from_secs(3_600),
    );
    assert_eq!(
        unbounded.breached(),
        None,
        "a deployment that set no budget has no deadline"
    );
}

/// TC-PORT-GUARD-2: the same call, asked for again and again, is a loop.
///
/// Upstream's detection rule: consecutive calls of the same tool with
/// canonically identical arguments. The count is of the whole batch a step
/// asked for rather than of one call, because a model alternating two tools is
/// looping and a per-call counter would reset on every alternation and never
/// fire.
///
/// Input: a limit of three, and the same batch asked for three times.
/// Expected: no breach on the first two, `Repeated` on the third.
#[test]
fn the_same_call_again_and_again_is_a_loop() {
    let mut watch = TurnWatch::start(TurnGuards {
        repeat_limit: Some(3),
        ..TurnGuards::default()
    });
    let batch = [call("echo", json!({ "text": "hi" }))];

    assert_eq!(watch.observe(&batch), None, "the first ask is not a repeat");
    assert_eq!(watch.observe(&batch), None, "nor the second");
    assert_eq!(watch.observe(&batch), Some(GuardBreach::Repeated));
}

/// TC-PORT-GUARD-3: doing something else resets the count.
///
/// A model that tries a thing, tries something different, and comes back is
/// working rather than looping. Resetting is what keeps the guard from firing
/// on a turn that is making progress slowly.
///
/// Input: two identical asks, a different one, then two identical again.
/// Expected: no breach anywhere, under a limit of three.
#[test]
fn doing_something_else_resets_the_count() {
    let mut watch = TurnWatch::start(TurnGuards {
        repeat_limit: Some(3),
        ..TurnGuards::default()
    });
    let same = [call("echo", json!({ "text": "hi" }))];
    let other = [call("echo", json!({ "text": "different" }))];

    assert_eq!(watch.observe(&same), None);
    assert_eq!(watch.observe(&same), None);
    assert_eq!(watch.observe(&other), None, "a different call is progress");
    assert_eq!(watch.observe(&same), None, "and the count started over");
    assert_eq!(watch.observe(&same), None);
}

/// TC-PORT-GUARD-4: the comparison is the tool and its arguments, not the call
/// id.
///
/// A provider mints a fresh id for every call, so a detector that compared ids
/// would find every call unique and never fire - the guard would be dead code
/// that looked alive. Arguments that differ are different work; a step that
/// asked for nothing is not a repeat of anything.
///
/// Input: identical calls under different ids; then different arguments; then
/// an empty batch between two identical ones.
/// Expected: the ids do not save the model from the guard; differing arguments
/// do; and the empty batch clears the count.
#[test]
fn the_comparison_is_the_call_and_not_its_id() {
    let mut watch = TurnWatch::start(TurnGuards {
        repeat_limit: Some(2),
        ..TurnGuards::default()
    });
    let mut first = call("echo", json!({ "text": "hi" }));
    first.id = "call_a".into();
    let mut second = call("echo", json!({ "text": "hi" }));
    second.id = "call_b".into();

    assert_eq!(watch.observe(&[first]), None);
    assert_eq!(
        watch.observe(&[second]),
        Some(GuardBreach::Repeated),
        "a fresh id per call must not make every call look unique"
    );

    let mut arguments = TurnWatch::start(TurnGuards {
        repeat_limit: Some(2),
        ..TurnGuards::default()
    });
    assert_eq!(
        arguments.observe(&[call("echo", json!({ "text": "a" }))]),
        None
    );
    assert_eq!(
        arguments.observe(&[call("echo", json!({ "text": "b" }))]),
        None,
        "different arguments are different work"
    );

    let mut interrupted = TurnWatch::start(TurnGuards {
        repeat_limit: Some(2),
        ..TurnGuards::default()
    });
    let same = [call("echo", json!({ "text": "hi" }))];
    assert_eq!(interrupted.observe(&same), None);
    assert_eq!(
        interrupted.observe(&[]),
        None,
        "a step that asked for nothing"
    );
    assert_eq!(
        interrupted.observe(&same),
        None,
        "cleared the count rather than continuing it"
    );
}

/// TC-GUARD-5: a limit that cannot mean anything is read as no limit.
///
/// A limit of one would stop a turn on its first tool call, and a limit of
/// zero on a turn that never made one. Neither is a bound a deployment could
/// have meant, and a harness that obeyed them literally would answer a
/// configuration mistake with a turn that can never use a tool.
///
/// Input: limits of zero and one, asked the same call repeatedly.
/// Expected: no breach, ever.
#[test]
fn a_limit_that_cannot_mean_anything_is_no_limit() {
    for limit in [0, 1] {
        let mut watch = TurnWatch::start(TurnGuards {
            repeat_limit: Some(limit),
            ..TurnGuards::default()
        });
        let batch = [call("echo", json!({ "text": "hi" }))];
        for _ in 0..5 {
            assert_eq!(watch.observe(&batch), None, "limit {limit} stopped a turn");
        }
    }
}

/// TC-GUARD-6: a guarded turn is a whole turn, and the summary is a summary.
///
/// The half of section 4.4.2 that a unit case cannot show. A bound the
/// deployment chose being reached is the bound working, so `agent.prompt`
/// answers a summary rather than an error - and the guard stops at a step
/// boundary, so the journal is balanced and section 4.6's state machine holds.
///
/// Input: the offline harness with a repeat limit of two, driving the mock
/// adapter that asks for the same echo call every step.
/// Expected: the turn returns an outcome; its reason is `Repeated`; the
/// journal's `turn/end` says `"repeated"`; every `step/start` has its
/// `step/end`, and every `tool/call` its `tool/result`.
#[tokio::test]
async fn a_guarded_turn_is_a_whole_turn() {
    let h = Harness::with_config(
        "guard-repeat",
        ToolRegistry::new().with(std::sync::Arc::new(tetanus_turn::tools::EchoTool)),
        TurnConfig {
            guards: TurnGuards {
                repeat_limit: Some(2),
                ..TurnGuards::default()
            },
            ..TurnConfig::default()
        },
    )
    .await;

    // A model that never settles: it asks for the same call every step, which
    // is the shape the repeat guard exists for and which no fixture adapter
    // produces on its own.
    let _looping = h.bus().on_waterfall::<LlmStream, _>(|_ev, _next| {
        Box::pin(async move {
            Ok(ModelResponse {
                content: String::new(),
                tool_calls: vec![call("echo", json!({ "text": "again" }))],
                finish_reason: "tool_calls".into(),
                ..Default::default()
            })
        })
    });

    let outcome = h
        .engine
        .run_turn("call the tool")
        .await
        .expect("a guarded turn answers a summary, not an error");

    assert_eq!(outcome.reason, StopReason::Repeated);

    let events = h.journal();
    let ended: Vec<&str> = events
        .iter()
        .filter(|e| e.ty == topic::TURN_END)
        .filter_map(|e| e.data["stop_reason"].as_str())
        .collect();
    assert_eq!(ended, ["repeated"], "the journal says which guard");

    let count = |ty: &str| events.iter().filter(|e| e.ty == ty).count();
    assert_eq!(
        count(topic::STEP_START),
        count(topic::STEP_END),
        "a guard stopped a step in flight: {events:#?}"
    );
    assert_eq!(
        count(topic::TOOL_CALL),
        count(topic::TOOL_RESULT),
        "a dispatched call was left without a result"
    );
    assert_eq!(count(topic::TURN_START), count(topic::TURN_END));
}

/// TC-GUARD-7: an unguarded turn is unchanged.
///
/// The default is both bounds absent, which has to behave exactly as every
/// build did before guards existed - a turn ends when the model stops asking
/// or the step budget runs out, and never because of a guard.
///
/// Input: the same harness with the default config.
/// Expected: the turn ends on the step budget, not on a guard.
#[tokio::test]
async fn an_unguarded_turn_is_unchanged() {
    let h = Harness::new("guard-absent").await;

    let outcome = h
        .engine
        .run_turn("call the tool")
        .await
        .expect("the turn ran");

    assert!(
        !matches!(outcome.reason, StopReason::Repeated | StopReason::TimedOut),
        "a guard fired with none configured: {:?}",
        outcome.reason
    );
}

/// TC-GUARD-8: the two reasons are distinct all the way to the wire.
///
/// The contract's whole argument for two reasons is that they need opposite
/// answers, which only holds if a reader can still tell them apart at the
/// boundary. Both travel as the growable enum's fallback (section 7.5), which
/// is where a spelling drifts if it is ever going to.
///
/// Input: each reason, as the engine names it.
/// Expected: `"timed-out"` and `"repeated"`, distinct, matching the words the
/// journal carries.
#[test]
fn the_two_reasons_stay_distinct() {
    assert_eq!(StopReason::TimedOut.as_str(), "timed-out");
    assert_eq!(StopReason::Repeated.as_str(), "repeated");
    assert_ne!(StopReason::TimedOut, StopReason::Repeated);
}
