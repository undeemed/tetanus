//! Test Design Specification: the closers a turn owes its journal.
//!
//! Features under test: what `TurnEngine::run_turn` writes when the steps end
//! in a failure rather than in an answer - the `step/end` for the step the
//! failure interrupted, the `turn/end` that reports it, and the reason that
//! `turn/end` carries. The success path is here too, as the guard on the split
//! that moved the closers out of the step loop.
//!
//! Not tested here: which failures a route retries before one ends the turn
//! (`upstream_retry_policy.rs`), who is offered a failed request
//! (`recovery_point.rs`), and what crash repair writes for a process that died
//! without closing anything (`upstream_repair.rs`). One case borrows repair's
//! reader to ask the opposite question: that a failed turn leaves nothing for
//! it to do.
//!
//! Environmental needs: none. No case reaches a network or an API key, and no
//! case waits on a clock.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use harness::Harness;

use tetanus_core::{EffectHandle, EventBus};
use tetanus_session::SessionEvent;
use tetanus_turn::events::{LlmStream, TurnStopping};
use tetanus_turn::llm::LlmError;
use tetanus_turn::repair::interrupted_turn_closers;
use tetanus_turn::{TurnError, FAILED_STOP_REASON};

/// TC-CLOSE-1: a turn a failure ended is closed on the journal like any other.
///
/// Input: the offline fixture with a route that fails its first model call, and
/// nothing listening on the recovery point.
/// Expected: `run_turn` answers the provider's failure; the journal read back
/// off disk ends `step/end`, `turn/end`; and that `turn/end` says turn 1, one
/// step, `stop_reason: "failed"` and no veto.
#[tokio::test]
async fn a_failed_turn_writes_its_own_end() {
    let h = Harness::new("close-failed").await;
    let (_attempts, _route) = fails_from(h.bus(), 0);

    let failed = h.engine.run_turn("this one fails").await;

    assert!(matches!(failed, Err(TurnError::Llm(_))), "{failed:?}");
    let journal = journal(&h);
    let types: Vec<&str> = journal.iter().map(|e| e.ty.as_str()).collect();
    assert_eq!(
        &types[types.len() - 2..],
        ["step/end", "turn/end"],
        "the step the failure interrupted closes, then the turn: {types:?}"
    );
    let end = journal.last().expect("the journal is not empty");
    assert_eq!(end.data["turn"], 1);
    assert_eq!(end.data["steps"], 1);
    assert_eq!(end.data["stop_reason"], FAILED_STOP_REASON);
    assert_eq!(end.data["stop_veto"], serde_json::Value::Null);
}

/// TC-CLOSE-2: the terminal checkpoint does not run for a turn that failed.
///
/// Input: the same failing route, with an `agent/turn-stopping` listener
/// counting the times it is asked.
/// Expected: the listener is never asked, and the closed turn reports no veto.
/// The checkpoint is where a listener may hold a turn open; a turn already
/// ended by a failure is not a turn anyone can hold open, and offering it would
/// invite a veto the engine would have to ignore.
#[tokio::test]
async fn a_failed_turn_is_not_offered_to_the_checkpoint() {
    let h = Harness::new("close-no-checkpoint").await;
    let (_attempts, _route) = fails_from(h.bus(), 0);
    let asked = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&asked);
    let _checkpoint = h.bus().on_serial::<TurnStopping, _>(move |_ev| {
        counted.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move { None })
    });

    let failed = h.engine.run_turn("this one fails").await;

    assert!(matches!(failed, Err(TurnError::Llm(_))), "{failed:?}");
    assert_eq!(asked.load(Ordering::Relaxed), 0, "asked nobody");
    assert!(
        !h.trace().iter().any(|t| t == "agent/turn-stopping"),
        "{:?}",
        h.trace()
    );
}

/// TC-CLOSE-3: the journal a failed turn leaves needs no repair.
///
/// Input: the journal of TC-CLOSE-1, handed to the reader `session.create` runs
/// over a journal it reopens.
/// Expected: no closers at all. The same journal used to look exactly like one
/// a process died in, and the next open would have written a `step/end` and a
/// `turn/end` reading `interrupted` for a turn that was never interrupted.
#[tokio::test]
async fn a_failed_turn_leaves_nothing_for_crash_repair() {
    let h = Harness::new("close-no-repair").await;
    let (_attempts, _route) = fails_from(h.bus(), 0);

    let failed = h.engine.run_turn("this one fails").await;
    assert!(matches!(failed, Err(TurnError::Llm(_))), "{failed:?}");

    let closers = interrupted_turn_closers(&journal(&h));
    assert!(
        closers.is_empty(),
        "a failed turn is not an interrupted one: {:?}",
        closers.iter().map(|c| c.ty).collect::<Vec<_>>()
    );
}

/// TC-CLOSE-4: a turn that ended itself is still closed once, with its own
/// reason.
///
/// Input: the fixture's mock provider, which answers a tool call and then an
/// answer, so the turn spends two steps and stops naturally.
/// Expected: one `turn/start` and one `turn/end`, a `step/end` for each of the
/// two `step/start`s, and a `turn/end` reading `natural` with two steps. The
/// closers moved out of the step loop in the change that gave a failed turn an
/// end, and this is what says the move changed nothing for a turn that works.
#[tokio::test]
async fn a_turn_that_ended_itself_is_closed_once() {
    let h = Harness::new("close-natural").await;

    let outcome = h.engine.run_turn("hello").await.expect("the turn ran");

    assert_eq!(outcome.steps, 2);
    let journal = journal(&h);
    assert_eq!(count(&journal, "turn/start"), 1);
    assert_eq!(count(&journal, "turn/end"), 1);
    assert_eq!(count(&journal, "step/start"), 2);
    assert_eq!(count(&journal, "step/end"), 2, "one end per step, not two");
    let end = journal.last().expect("the journal is not empty");
    assert_eq!(end.ty, "turn/end");
    assert_eq!(end.data["steps"], 2);
    assert_eq!(end.data["stop_reason"], "natural");
}

/// TC-CLOSE-5: the closers of a turn that failed late name the step it was on.
///
/// Input: a route that answers the first model call and fails the second, so
/// the failure lands on the second step of the turn.
/// Expected: the first step closed itself, the closer writes `step/end` for
/// step 2, and `turn/end` reports two steps spent and `stop_reason: "failed"`.
#[tokio::test]
async fn a_late_failure_closes_the_step_it_was_on() {
    let h = Harness::new("close-late").await;
    let (attempts, _route) = fails_from(h.bus(), 1);

    let failed = h.engine.run_turn("fail on the second call").await;

    assert!(matches!(failed, Err(TurnError::Llm(_))), "{failed:?}");
    assert_eq!(
        attempts.load(Ordering::Relaxed),
        2,
        "one answer, one failure"
    );
    let journal = journal(&h);
    assert_eq!(count(&journal, "step/end"), 2);
    let ends: Vec<&SessionEvent> = journal.iter().filter(|e| e.ty == "step/end").collect();
    assert_eq!(ends[1].data["step"], 2, "the closer names the open step");
    let end = journal.last().expect("the journal is not empty");
    assert_eq!(end.ty, "turn/end");
    assert_eq!(end.data["steps"], 2);
    assert_eq!(end.data["stop_reason"], FAILED_STOP_REASON);
}

/// A route that answers its first `answers` calls and fails every call after
/// them, counting every call it was asked to make.
fn fails_from(bus: &EventBus, answers: u32) -> (Arc<AtomicU32>, EffectHandle) {
    let attempts = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&attempts);
    let handle = bus.on_waterfall::<LlmStream, _>(move |ev, next| {
        let counted = Arc::clone(&counted);
        Box::pin(async move {
            if counted.fetch_add(1, Ordering::Relaxed) < answers {
                return next.run(ev).await;
            }
            Err(LlmError::Provider {
                status: 503,
                message: "upstream is down".into(),
            })
        })
    });
    (attempts, handle)
}

/// The journal read back off disk rather than out of the log's memory: what a
/// surface or a resumed session sees is the file.
fn journal(h: &Harness) -> Vec<SessionEvent> {
    tetanus_session::replay(&h.log_path).expect("the journal reads back")
}

fn count(journal: &[SessionEvent], ty: &str) -> usize {
    journal.iter().filter(|e| e.ty == ty).count()
}
