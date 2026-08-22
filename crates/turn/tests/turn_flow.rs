//! Test Design Specification: turn-flow conformance.
//!
//! Feature under test: the turn engine emits the complete documented dsh event
//! sequence, in order, across both event domains - durable session events on
//! the log and live extension points on the bus.
//!
//! Approach: one trace is assembled from bus listeners only. Durable events are
//! observed through the `session/event` emit broadcast; live events through a
//! listener on each waterfall and the serial checkpoint. Both domains land in
//! one ordered vector, so the assertion covers order *across* domains, not just
//! within one.
//!
//! Reference: upstream `docs/architecture.md` ("Turn flow") and
//! `docs/agent-lifecycle.md`. `docs/turn-flow.md` in this repo records the
//! reconciliation between the two and is the authoritative expected sequence.
//!
//! Pass criteria: the observed trace equals the expected vector exactly.
//! Fail criteria: any missing, extra, or reordered event.

mod harness;

use harness::{Harness, MOCK_TURN_FLOW};

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use tetanus_core::events::BoxFuture;
use tetanus_session::SessionEvent;
use tetanus_turn::events::{LlmStream, PreStep, PreStepDecision, StopReason, TurnStopVeto};
use tetanus_turn::llm::{ModelResponse, StreamChunk};
use tetanus_turn::log::topic;

/// TC-TURN-1: one full turn emits the complete documented sequence, in order.
///
/// Input: `run_turn("run one full turn")` on the mock adapter with the `echo`
/// tool registered.
/// Expected: the trace equals [`MOCK_TURN_FLOW`] exactly; the turn stops
/// naturally after two steps and answers with the echoed text.
#[tokio::test]
async fn emits_the_full_documented_event_sequence() {
    let h = Harness::new("turn-flow").await;

    let outcome = h.engine.run_turn("run one full turn").await.unwrap();

    assert_eq!(h.trace(), MOCK_TURN_FLOW, "documented turn flow");
    assert_eq!(outcome.steps, 2);
    assert_eq!(outcome.reason, StopReason::Natural);
    assert_eq!(outcome.content, "You said: run one full turn");
    assert_eq!(outcome.stop_veto, None);
}

/// TC-TURN-2: every durable event of the turn reaches the JSONL journal, in the
/// same order and with contiguous seq numbers, and reads back identically.
///
/// Expected: the replayed types equal the durable subsequence of the trace;
/// `seq` is `0..n`; the replayed events equal the in-memory log.
#[tokio::test]
async fn every_durable_event_persists_and_replays() {
    let h = Harness::new("durable").await;
    h.engine.run_turn("run one full turn").await.unwrap();
    h.engine.flush().await.unwrap();

    let durable: Vec<&str> = MOCK_TURN_FLOW
        .iter()
        .copied()
        .filter(|t| is_durable(t))
        .collect();

    let replayed = tetanus_session::replay(&h.log_path).unwrap();
    let types: Vec<&str> = replayed.iter().map(|e| e.ty.as_str()).collect();

    assert_eq!(
        types, durable,
        "journal holds every durable event, in order"
    );
    for (i, event) in replayed.iter().enumerate() {
        assert_eq!(event.seq, i as u64, "seq is the position in the log");
        assert!(event.time > 0, "every event carries an epoch-ms timestamp");
    }
    assert_eq!(
        replayed,
        h.engine.log().events(),
        "replay equals the live log"
    );
}

/// TC-TURN-3: an `assistant/message` cites the exact `assistant/chunk` events
/// that built it.
///
/// Expected: for each of the two steps, `sourceEventSeqs` lists the seqs of the
/// chunk events that immediately precede it, in order.
#[tokio::test]
async fn assistant_messages_cite_their_chunks() {
    let h = Harness::new("sources").await;
    h.engine.run_turn("run one full turn").await.unwrap();

    let events = tetanus_session::replay(&h.log_path).unwrap();
    let messages: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| e.ty == topic::ASSISTANT_MESSAGE)
        .collect();
    assert_eq!(messages.len(), 2);

    for message in messages {
        let cited = message.source_event_seqs.clone().expect("cited sources");
        assert!(!cited.is_empty(), "the mock stream is never empty");
        for seq in &cited {
            assert_eq!(events[*seq as usize].ty, topic::ASSISTANT_CHUNK);
            assert!(*seq < message.seq, "an event only cites earlier events");
        }
        let expected: Vec<u64> = (cited[0]..message.seq).collect();
        assert_eq!(
            cited, expected,
            "the cited chunks are the ones just streamed"
        );
    }
}

/// TC-TURN-4: a rejected first claim still closes a durable turn that spent no
/// step, so the log records the attempt.
///
/// Input: an `agent/pre-step` listener that rejects without delegating.
/// Expected: trace `["turn/start", "agent/pre-step", "turn/end"]`; zero steps;
/// stop reason `pre-step-rejected`; no `agent/turn-stopping`.
#[tokio::test]
async fn a_rejected_first_claim_closes_a_turn_with_no_step() {
    let h = Harness::new("rejected").await;

    let _reject = h.bus().on_waterfall::<PreStep, _>(|_ev, _next| {
        Box::pin(async move { PreStepDecision::Reject("policy said no".into()) })
    });

    let outcome = h.engine.run_turn("blocked").await.unwrap();

    assert_eq!(h.trace(), vec!["turn/start", "agent/pre-step", "turn/end"]);
    assert_eq!(outcome.steps, 0);
    assert_eq!(outcome.reason, StopReason::PreStepRejected);

    let events = tetanus_session::replay(&h.log_path).unwrap();
    let types: Vec<&str> = events.iter().map(|e| e.ty.as_str()).collect();
    assert_eq!(types, vec![topic::TURN_START, topic::TURN_END]);
}

/// TC-TURN-9: a claim rejected after a step has run closes the turn the work
/// already entered, and keeps that work.
///
/// Upstream: `coverage-edges.spec.ts`, "closes an entered turn as blocked when
/// its next step is rejected". Upstream needs a plugin to inject a message so a
/// second step is proposed at all; the mock's first step asks for a tool, so
/// tetanus is already owed one.
///
/// Input: an `agent/pre-step` listener that delegates the first claim and
/// rejects the second.
/// Expected: two claims, one step, stop reason `pre-step-rejected` in the
/// answer and on the journal's `turn/end`, the tool result of the step that
/// did run still on the journal, and the terminal
/// checkpoint fired between the refused claim and `turn/end`. A rejection ends
/// the turn; it does not undo the step before it, and a turn that spent a step
/// still reaches `agent/turn-stopping` whatever ended it - TC-TURN-4 pins the
/// other side, where a turn that never entered a step has no checkpoint to
/// run.
#[tokio::test]
async fn a_claim_rejected_after_a_step_closes_the_turn_it_entered() {
    let h = Harness::new("rejected-next-claim").await;

    let claims = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&claims);
    let _reject = h.bus().on_waterfall::<PreStep, _>(move |ev, next| {
        let counted = Arc::clone(&counted);
        Box::pin(async move {
            if counted.fetch_add(1, Ordering::AcqRel) == 0 {
                next.run(ev).await
            } else {
                PreStepDecision::Reject("no second step".into())
            }
        })
    });

    let outcome = h.engine.run_turn("go").await.unwrap();

    assert_eq!(
        claims.load(Ordering::Acquire),
        2,
        "the second claim was made"
    );
    assert_eq!(outcome.steps, 1);
    assert_eq!(outcome.reason, StopReason::PreStepRejected);

    let trace = h.trace();
    assert_eq!(
        trace.iter().filter(|t| *t == "step/start").count(),
        1,
        "only the entered step ran"
    );
    let tail: Vec<&str> = trace
        .iter()
        .rev()
        .take(3)
        .rev()
        .map(String::as_str)
        .collect();
    assert_eq!(
        tail,
        vec!["agent/pre-step", "agent/turn-stopping", "turn/end"],
        "the refused claim, then the checkpoint a spent turn still runs, then the close: {trace:?}"
    );

    let events = h.engine.log().events();
    assert!(
        events.iter().any(|e| e.ty == topic::TOOL_RESULT),
        "the work the entered step did is kept"
    );
    let end = events
        .iter()
        .rev()
        .find(|e| e.ty == topic::TURN_END)
        .expect("the turn is closed on the journal");
    assert_eq!(end.data["stop_reason"], "pre-step-rejected");
    assert_eq!(end.data["steps"], 1);
}

/// TC-TURN-10: a first claim rewritten to nothing closes the turn with no step.
///
/// An empty enter is not a rejection, so nothing says no; but a step with no
/// message to send is a request the model has no reason to answer. The engine
/// treats it as the claim it is - one that entered nothing - and closes the
/// turn the same way a rejection does, rather than dispatching an empty
/// conversation.
///
/// Input: an `agent/pre-step` listener that enters an empty message list.
/// Expected: zero steps, stop reason `pre-step-rejected`, and a journal holding
/// only the two ends of the turn.
#[tokio::test]
async fn a_first_claim_rewritten_to_nothing_closes_the_turn_with_no_step() {
    let h = Harness::new("entered-nothing").await;

    let _empty = h.bus().on_waterfall::<PreStep, _>(|_ev, _next| {
        Box::pin(async move { PreStepDecision::Enter(Vec::new()) })
    });

    let outcome = h.engine.run_turn("go").await.unwrap();

    assert_eq!(outcome.steps, 0);
    assert_eq!(outcome.reason, StopReason::PreStepRejected);
    assert_eq!(h.trace(), vec!["turn/start", "agent/pre-step", "turn/end"]);

    let events = tetanus_session::replay(&h.log_path).unwrap();
    let types: Vec<&str> = events.iter().map(|e| e.ty.as_str()).collect();
    assert_eq!(types, vec![topic::TURN_START, topic::TURN_END]);
}

/// TC-TURN-5: an `llm/stream` listener that does not delegate replaces the
/// provider call entirely; the adapter never runs.
///
/// Expected: the turn completes in one step with the replacement text, and no
/// `assistant/chunk` from the mock adapter reaches the log.
#[tokio::test]
async fn an_llm_stream_listener_can_replace_the_provider_call() {
    let h = Harness::new("replaced").await;

    let _replace = h.bus().on_waterfall::<LlmStream, _>(|ev, _next| {
        Box::pin(async move {
            ev.sink
                .chunk(StreamChunk::Text {
                    delta: "canned".into(),
                })
                .await?;
            Ok(ModelResponse {
                content: "canned".into(),
                finish_reason: "stop".into(),
                ..ModelResponse::default()
            })
        })
    });

    let outcome = h.engine.run_turn("anything").await.unwrap();

    assert_eq!(outcome.content, "canned");
    assert_eq!(outcome.steps, 1);
    let chunks: Vec<String> = tetanus_session::replay(&h.log_path)
        .unwrap()
        .into_iter()
        .filter(|e| e.ty == topic::ASSISTANT_CHUNK)
        .map(|e| e.data["delta"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(chunks, vec!["canned"]);
}

/// TC-TURN-6: `agent/turn-stopping` is serial, and a listener that bails is
/// recorded on the closing `turn/end`.
///
/// Expected: `stop_veto == Some("more work owed")`, and the same string appears
/// in the `turn/end` payload.
#[tokio::test]
async fn a_turn_stopping_listener_can_bail() {
    let h = Harness::new("veto").await;

    let _veto = h
        .bus()
        .on_serial::<tetanus_turn::events::TurnStopping, _>(|_ev| {
            Box::pin(async move {
                Some(TurnStopVeto {
                    reason: "more work owed".into(),
                })
            }) as BoxFuture<'_, _>
        });

    let outcome = h.engine.run_turn("run one full turn").await.unwrap();

    assert_eq!(outcome.stop_veto.as_deref(), Some("more work owed"));
    let end = tetanus_session::replay(&h.log_path)
        .unwrap()
        .into_iter()
        .find(|e| e.ty == topic::TURN_END)
        .expect("turn/end");
    assert_eq!(end.data["stop_veto"], "more work owed");
}

/// TC-TURN-7: turn numbers are monotonic within one engine and each turn's
/// durable events append to the same journal.
///
/// Expected: turns 1 and 2; the journal holds two `turn/start` and two
/// `turn/end` events, and every seq stays contiguous.
#[tokio::test]
async fn turns_are_numbered_and_appended_to_one_journal() {
    let h = Harness::new("two-turns").await;

    let first = h.engine.run_turn("first").await.unwrap();
    let second = h.engine.run_turn("second").await.unwrap();

    assert_eq!((first.turn, second.turn), (1, 2));
    let events = tetanus_session::replay(&h.log_path).unwrap();
    assert_eq!(
        events.iter().filter(|e| e.ty == topic::TURN_START).count(),
        2
    );
    assert_eq!(events.iter().filter(|e| e.ty == topic::TURN_END).count(), 2);
}

/// TC-TURN-8: the second turn on one engine runs the same documented sequence
/// as the first, and answers its own prompt.
///
/// A request carries the whole conversation, so an adapter that asks the
/// conversation a question about this step reads an earlier turn's answer to
/// it. Issue #140 was exactly that: every turn after the first echoed turn 1's
/// text and called no tool, because a tool result anywhere in the history read
/// as "this step is already answered".
///
/// Input: two `run_turn` calls on one engine, with different prompts.
/// Expected: the trace equals [`MOCK_TURN_FLOW`] twice over - so turn 2 calls
/// the tool and steps twice, exactly as turn 1 does - and turn 2 answers with
/// its own text.
#[tokio::test]
async fn a_second_turn_runs_the_same_sequence_as_the_first() {
    let h = Harness::new("second-turn").await;

    h.engine.run_turn("first").await.unwrap();
    let second = h.engine.run_turn("second").await.unwrap();

    let twice: Vec<&str> = MOCK_TURN_FLOW
        .iter()
        .chain(MOCK_TURN_FLOW.iter())
        .copied()
        .collect();
    assert_eq!(h.trace(), twice, "turn 2 runs the documented turn too");
    assert_eq!(second.steps, 2);
    assert_eq!(second.content, "You said: second");
}

/// Which topics of [`MOCK_TURN_FLOW`] are durable session events rather than
/// in-memory extension points. `request/context` is the request envelope a
/// step writes before it dispatches, and it is on the journal like the rest.
fn is_durable(topic: &str) -> bool {
    topic.starts_with("turn/")
        || topic.starts_with("step/")
        || topic == "user/message"
        || topic == "request/context"
        || topic.starts_with("assistant/")
        || topic.starts_with("tool/")
}
