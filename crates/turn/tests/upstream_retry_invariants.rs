//! Test Design Specification: what a journal must say about a retry chain.
//!
//! Features under test: the durable invariants upstream
//! `packages/llm/llm-retry/tests/invariant.spec.ts` enforces when a record is
//! appended - numbering scoped to one step of one turn, one started record per
//! scheduled attempt, every record inside the turn and step that is open, and
//! a bound that counts the records the journal already carries. tetanus has no
//! append-time validator, so each claim is asserted against the journal a real
//! turn writes.
//!
//! Approach: the shared offline fixture with an `llm/stream` listener that
//! fails the first attempts of every step. One mock turn is two steps, so a
//! single run produces one chain per step and a second prompt produces one per
//! turn. Waits are single milliseconds; the numbers themselves are pinned by
//! `upstream_retry_policy.rs`.
//!
//! Features NOT tested here: which failures are worth another attempt and how
//! long the wait is (`upstream_retry_policy.rs`), and what the executor does
//! with one failure (`upstream_retry_executor.rs`).
//!
//! Environmental needs: none. No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use harness::Harness;
use tetanus_core::{EffectHandle, EventBus};
use tetanus_session::SessionEvent;
use tetanus_turn::events::LlmStream;
use tetanus_turn::llm::mock::PROVIDER;
use tetanus_turn::llm::retry::{install, Backoff, RetryPolicy, RETRY_EVENT, RETRY_STARTED_EVENT};
use tetanus_turn::llm::{LlmError, ModelRequest, Role};
use tetanus_turn::log::topic;
use tetanus_turn::TurnError;

/// TC-PORT-RETRYINV-1: a new step starts its own chain.
///
/// Upstream: `invariant.spec.ts`, "binds retry numbering to the provider
/// policy and resets it for a new step".
///
/// Input: one turn whose every step fails its first attempt with 503, under a
/// policy that retries `SERVER` twice.
/// Expected: the turn completes, and the journal carries one scheduled retry
/// per step, each numbered one. The second step is not the first step's second
/// attempt, so a step never inherits a bound another step spent.
#[tokio::test]
async fn numbering_restarts_in_the_next_step() {
    let h = Harness::new("retryinv-step-reset").await;
    let (attempts, _flaky) = fails_first(h.bus(), 1);
    let _executor = executor(&h);

    h.engine.run_turn("retry once per step").await.expect("ran");

    assert_eq!(
        attempts.load(Ordering::Relaxed),
        4,
        "two steps, one retry each"
    );
    assert_eq!(chain(&h, RETRY_EVENT), [(1, 1, 1), (1, 2, 1)]);
}

/// TC-PORT-RETRYINV-2: a new turn starts its own chain.
///
/// Upstream: `invariant.spec.ts`, "starts a fresh retry chain after incomplete
/// predecessor boundaries" - a chain belongs to the turn it was opened in.
///
/// Input: two prompts on one session, each step failing its first attempt.
/// Expected: four scheduled retries, numbered one in each step of each turn.
/// The second turn does not open already spent, which is what a session that
/// has retried before would otherwise do.
#[tokio::test]
async fn numbering_restarts_in_the_next_turn() {
    let h = Harness::new("retryinv-turn-reset").await;
    let (_attempts, _flaky) = fails_first(h.bus(), 1);
    let _executor = executor(&h);

    h.engine
        .run_turn("first")
        .await
        .expect("the first turn ran");
    h.engine
        .run_turn("second")
        .await
        .expect("the second turn ran");

    assert_eq!(
        chain(&h, RETRY_EVENT),
        [(1, 1, 1), (1, 2, 1), (2, 1, 1), (2, 2, 1)]
    );
}

/// TC-PORT-RETRYINV-3: one started record per scheduled attempt, after it.
///
/// Upstream: `invariant.spec.ts`, "validates retry-started correlation and
/// uniqueness".
///
/// Input: one turn whose every step fails its first two attempts.
/// Expected: the journal alternates scheduled record and started record, in
/// step order and then in attempt order. Every started record pairs the
/// attempt scheduled immediately before it, no attempt is announced twice, and
/// none is announced before it was promised.
#[tokio::test]
async fn every_started_record_pairs_one_scheduled_attempt() {
    let h = Harness::new("retryinv-pairing").await;
    let (_attempts, _flaky) = fails_first(h.bus(), 2);
    let _executor = executor(&h);

    h.engine
        .run_turn("retry twice per step")
        .await
        .expect("ran");

    let pairs: Vec<(String, Attempt)> = journal(&h)
        .iter()
        .filter(|event| event.ty == RETRY_EVENT || event.ty == RETRY_STARTED_EVENT)
        .map(|event| (event.ty.clone(), attempt(event)))
        .collect();
    assert_eq!(
        pairs,
        [
            (RETRY_EVENT.to_string(), (1, 1, 1)),
            (RETRY_STARTED_EVENT.to_string(), (1, 1, 1)),
            (RETRY_EVENT.to_string(), (1, 1, 2)),
            (RETRY_STARTED_EVENT.to_string(), (1, 1, 2)),
            (RETRY_EVENT.to_string(), (1, 2, 1)),
            (RETRY_STARTED_EVENT.to_string(), (1, 2, 1)),
            (RETRY_EVENT.to_string(), (1, 2, 2)),
            (RETRY_STARTED_EVENT.to_string(), (1, 2, 2)),
        ]
    );
}

/// TC-PORT-RETRYINV-4: every record lands inside the turn and step it names.
///
/// Upstream: `invariant.spec.ts`, "rejects a record outside an open turn" and
/// the location half of the retry-started case.
///
/// Input: the same two-retries-per-step turn, read back as a whole journal.
/// Expected: at each retry record, a turn and a step are open, and they are
/// the ones the record names; and the run opened two steps, not six. A record
/// that named a closed step would be a wait nobody is inside, and a retry that
/// opened a step would make one failed request into two.
#[tokio::test]
async fn every_record_names_the_turn_and_step_that_are_open() {
    let h = Harness::new("retryinv-inside").await;
    let (_attempts, _flaky) = fails_first(h.bus(), 2);
    let _executor = executor(&h);

    h.engine
        .run_turn("retry inside the step")
        .await
        .expect("ran");

    let mut open: (Option<u64>, Option<u64>) = (None, None);
    let mut checked = 0;
    for event in journal(&h) {
        match event.ty.as_str() {
            topic::TURN_START => open.0 = event.data["turn"].as_u64(),
            topic::TURN_END => open.0 = None,
            topic::STEP_START => open.1 = event.data["step"].as_u64(),
            topic::STEP_END => open.1 = None,
            RETRY_EVENT | RETRY_STARTED_EVENT => {
                let named = (event.data["turn"].as_u64(), event.data["step"].as_u64());
                assert_eq!(open, named, "{} at seq {}", event.ty, event.seq);
                checked += 1;
            }
            _ => {}
        }
    }
    assert_eq!(checked, 8, "four scheduled attempts, each with its start");
    assert_eq!(
        h.trace()
            .iter()
            .filter(|topic| *topic == topic::STEP_START)
            .count(),
        2,
        "a retry re-sends inside the step that failed"
    );
}

/// TC-PORT-RETRYINV-5: the bound counts the records the journal already holds.
///
/// Upstream: `invariant.spec.ts`, "keeps one retry identity per
/// provider-policy chain" - a chain is the journal's, not a listener's.
///
/// Input: a journal that already carries retry one of this turn and step, as a
/// session resumed mid-chain does, and a route with three failures in it under
/// a policy allowing two retries. The route has one failure more than the case
/// needs, so a bound that lost count spends it and fails here rather than
/// running out of route and passing.
/// Expected: exactly one more attempt goes out, numbered two, and then the
/// failure stands. A count kept in memory would have granted two.
#[tokio::test]
async fn a_chain_read_back_off_the_journal_keeps_its_bound() {
    let h = Harness::new("retryinv-resumed").await;
    let (attempts, _flaky) = fails_first(h.bus(), 3);
    let _executor = executor(&h);
    h.engine
        .log()
        .append(
            RETRY_EVENT,
            serde_json::json!({
                "turn": 1,
                "step": 1,
                "provider": PROVIDER,
                "code": "SERVER",
                "message": "spent before this process started",
                "retry": 1,
                "max_retries": 2,
                "delay_ms": 2,
            }),
        )
        .expect("the journal takes the earlier record");

    let failed = h.engine.run_turn("already half spent").await;

    assert!(matches!(failed, Err(TurnError::Llm(_))), "{failed:?}");
    assert_eq!(attempts.load(Ordering::Relaxed), 2, "one attempt was left");
    assert_eq!(chain(&h, RETRY_EVENT), [(1, 1, 1), (1, 1, 2)]);
}

/// One scheduled attempt, as the journal reports it: turn, step, retry number.
type Attempt = (u64, u64, u64);

/// A provider that fails the first `per_step` attempts of every step, so a
/// chain opens more than once in a run.
///
/// The step is read off the request rather than counted, because a retry
/// re-sends the same request: the whole conversation is there, so the turn is
/// how many user messages it holds, and a step that already has this step's
/// tool result is the one that answers.
fn fails_first(bus: &EventBus, per_step: u32) -> (Arc<AtomicU32>, EffectHandle) {
    let attempts = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&attempts);
    let failed: Arc<Mutex<BTreeMap<(u64, u64), u32>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let handle = bus.on_waterfall::<LlmStream, _>(move |ev, next| {
        let counted = Arc::clone(&counted);
        let failed = Arc::clone(&failed);
        let key = step_of(&ev.request);
        Box::pin(async move {
            counted.fetch_add(1, Ordering::Relaxed);
            let spent = {
                let mut failed = failed.lock().expect("failures");
                let spent = failed.entry(key).or_insert(0);
                if *spent < per_step {
                    *spent += 1;
                    true
                } else {
                    false
                }
            };
            if spent {
                return Err(LlmError::Provider {
                    status: 503,
                    message: "upstream is down".into(),
                    retry_after_ms: None,
                });
            }
            next.run(ev).await
        })
    });
    (attempts, handle)
}

fn step_of(request: &ModelRequest) -> (u64, u64) {
    let turn = request
        .messages
        .iter()
        .filter(|message| message.role == Role::User)
        .count() as u64;
    let step = match request.messages.last() {
        Some(message) if message.role == Role::Tool => 2,
        _ => 1,
    };
    (turn, step)
}

/// The executor every case installs: two retries of `SERVER` on the mock
/// route, waited out in single milliseconds.
fn executor(h: &Harness) -> EffectHandle {
    install(
        h.bus(),
        Arc::clone(h.engine.log()),
        PROVIDER,
        RetryPolicy::Normal {
            max_retries: 2,
            retryable_codes: vec!["SERVER".to_string()],
            backoff: Backoff {
                initial_delay_ms: 2.0,
                max_delay_ms: 8.0,
                jitter_ratio: 0.0,
            },
        },
        Arc::new(|| 0.5),
    )
}

/// The journal as a surface or a resumed session reads it: off the file, in
/// the order it was written.
fn journal(h: &Harness) -> Vec<SessionEvent> {
    tetanus_session::replay(&h.log_path).expect("the journal reads back")
}

/// The attempts of one record type, in journal order.
fn chain(h: &Harness, ty: &str) -> Vec<Attempt> {
    journal(h)
        .iter()
        .filter(|event| event.ty == ty)
        .map(attempt)
        .collect()
}

fn attempt(event: &SessionEvent) -> Attempt {
    let field = |name: &str| event.data[name].as_u64().unwrap_or_default();
    (field("turn"), field("step"), field("retry"))
}
