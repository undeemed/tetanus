//! Test Design Specification: what every journal this engine writes satisfies.
//!
//! Features under test: the enclosure and numbering rules upstream's session
//! store enforces on each append, in
//! `packages/core/session/tests/invariant.spec.ts`: turns numbered from one
//! without a gap, one turn and one step open at a time, every record inside
//! the open step, a record that names a turn and a step naming the open ones,
//! and a tool result answering a call from its own step.
//!
//! Approach: one fold states every rule once, and each case runs a differently
//! shaped turn through it - a plain turn, three turns on one session, a tool
//! that panics, a provider failure that ends the turn, and an interrupt at the
//! step boundary. The rules are checked against what the driver writes rather
//! than against a hand-built journal, so a case fails when the driver changes
//! and not when a fixture does.
//!
//! Features NOT tested here: the exact sequence one turn emits
//! (`turn_flow.rs`), what closes a turn that failed (`turn_close.rs`), and the
//! properties of any journal at all, written by anything (`properties.rs`).
//! Upstream's append-time refusals have nothing to restate: tetanus has no
//! validator to refuse an append, so the claim is about the writer.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

// The fixture is shared, and this binary reads the journal rather than the
// bus, so the parts of it these cases do not reach are not dead code.
#[allow(dead_code)]
mod harness;

use std::sync::Arc;
use std::time::Duration;

use harness::Harness;
use tetanus_core::{EffectHandle, EventBus};
use tetanus_session::SessionEvent;
use tetanus_turn::events::LlmStream;
use tetanus_turn::llm::LlmError;
use tetanus_turn::log::topic;
use tetanus_turn::tools::{Tool, ToolError, ToolOutcome, ToolRegistry, ToolSchema};
use tetanus_turn::TurnError;

/// TC-PORT-SESSINV-1: the ordinary turn breaks no rule.
///
/// Upstream: `invariant.spec.ts`, "accepts a well-formed turn, step, and tool
/// sequence".
///
/// Input: one mock turn - two steps, a tool call and its result.
/// Expected: no breach, and the fold saw the two steps and the one call it
/// was meant to check rather than an empty journal.
#[tokio::test]
async fn the_ordinary_turn_breaks_no_rule() {
    let h = Harness::new("sessinv-plain").await;

    h.engine.run_turn("hello").await.expect("the turn ran");

    let journal = journal(&h);
    assert_eq!(breaches(&journal), Vec::<String>::new());
    assert_eq!(count(&journal, topic::STEP_START), 2, "two steps ran");
    assert_eq!(count(&journal, topic::TOOL_CALL), 1, "one call was made");
}

/// TC-PORT-SESSINV-2: numbering runs on across turns and restarts inside them.
///
/// Upstream: `invariant.spec.ts`, "enforces turn numbering and core execution
/// enclosure" and "enforces open-step identity and numbering".
///
/// Input: three prompts on one session.
/// Expected: no breach; the turns are one, two and three; and each turn's
/// steps are one and two. A turn that reopened a number, or a step that
/// carried on counting from the last turn, is a breach the fold names.
#[tokio::test]
async fn numbering_runs_on_across_turns_and_restarts_inside_them() {
    let h = Harness::new("sessinv-three-turns").await;

    for prompt in ["first", "second", "third"] {
        h.engine.run_turn(prompt).await.expect("the turn ran");
    }

    let journal = journal(&h);
    assert_eq!(breaches(&journal), Vec::<String>::new());
    assert_eq!(numbers(&journal, topic::TURN_START, "turn"), [1, 2, 3]);
    assert_eq!(
        numbers(&journal, topic::STEP_START, "step"),
        [1, 2, 1, 2, 1, 2]
    );
}

/// TC-PORT-SESSINV-3: a tool that panics leaves the journal well formed.
///
/// Upstream: `invariant.spec.ts`, "keeps fresh tool-result appends open-step
/// checked".
///
/// Input: the tool the model calls panics, so the failure is contained and
/// reported to the model as a result.
/// Expected: no breach - the result answers the call made in its own step -
/// and the turn still ends normally.
#[tokio::test]
async fn a_tool_that_panics_leaves_the_journal_well_formed() {
    let h = Harness::with_tools(
        "sessinv-tool-panic",
        ToolRegistry::new().with(Arc::new(Boom)),
    )
    .await;

    h.engine
        .run_turn("call the tool")
        .await
        .expect("the turn ran");

    let journal = journal(&h);
    assert_eq!(breaches(&journal), Vec::<String>::new());
    assert_eq!(count(&journal, topic::TOOL_RESULT), 1, "the call settled");
}

/// TC-PORT-SESSINV-4: a turn a failure ended is closed, not abandoned.
///
/// Upstream: `invariant.spec.ts`, "enforces turn numbering and core execution
/// enclosure" - the half that refuses a record outside an open turn, which a
/// journal ending mid-turn would make of the next turn's first record.
///
/// Input: a provider that fails every call, under no retry policy.
/// Expected: the turn fails, and the journal still breaks no rule: the step
/// the failure interrupted is closed, then the turn, so nothing is left open.
#[tokio::test]
async fn a_turn_a_failure_ended_is_closed_not_abandoned() {
    let h = Harness::new("sessinv-failed").await;
    let _provider = always_fails(h.bus());

    let failed = h.engine.run_turn("this will fail").await;

    assert!(matches!(failed, Err(TurnError::Llm(_))), "{failed:?}");
    let journal = journal(&h);
    assert_eq!(breaches(&journal), Vec::<String>::new());
    assert_eq!(count(&journal, topic::TURN_END), 1, "the turn was closed");
}

/// TC-PORT-SESSINV-5: an interrupted turn is closed at the step boundary.
///
/// Upstream: `invariant.spec.ts`, "allows not-started repair results and
/// unresolved calls at step end" - a turn stopped part way is a well-formed
/// journal, not a torn one.
///
/// Input: a cancel that arrives once the first step has closed.
/// Expected: the turn ends, the journal breaks no rule, and it ends with
/// nothing open.
#[tokio::test]
async fn an_interrupted_turn_is_closed_at_the_step_boundary() {
    let h = Harness::new("sessinv-interrupted").await;

    let cancel = async {
        until_written(&h, topic::STEP_END).await;
        h.engine.cancel();
    };
    let (ran, ()) = tokio::join!(h.engine.run_turn("stop after a step"), cancel);
    ran.expect("an interrupted turn is an outcome, not an error");

    let journal = journal(&h);
    assert_eq!(breaches(&journal), Vec::<String>::new());
    assert_eq!(count(&journal, topic::TURN_END), 1, "the turn was closed");
}

/// Every rule the suite states, applied to one journal, answering the breaches
/// it found in journal order.
///
/// It is one fold rather than one assertion per case because upstream states
/// the same rules once, in the validator every append passes through. A case
/// here chooses the journal; the rules do not move.
fn breaches(events: &[SessionEvent]) -> Vec<String> {
    let mut turn: Option<u64> = None;
    let mut step: Option<u64> = None;
    let mut turns = 0;
    let mut steps = 0;
    let mut calls: Vec<String> = Vec::new();
    let mut found = Vec::new();
    let mut report = |seq: u64, what: String| found.push(format!("seq {seq}: {what}"));

    for event in events {
        let seq = event.seq;
        match event.ty.as_str() {
            topic::TURN_START => {
                let opened = number(event, "turn");
                if let Some(open) = turn {
                    report(seq, format!("turn {opened} opened inside turn {open}"));
                }
                if opened != turns + 1 {
                    report(seq, format!("turn {opened} follows turn {turns}"));
                }
                turn = Some(opened);
                turns = opened;
                steps = 0;
            }
            topic::TURN_END => {
                if turn != Some(number(event, "turn")) {
                    report(seq, format!("{} closes no open turn", event.ty));
                }
                if let Some(open) = step {
                    report(seq, format!("the turn ends with step {open} open"));
                }
                turn = None;
                step = None;
            }
            topic::STEP_START => {
                let opened = number(event, "step");
                if turn != Some(number(event, "turn")) {
                    report(seq, format!("step {opened} opens outside its turn"));
                }
                if let Some(open) = step {
                    report(seq, format!("step {opened} opened inside step {open}"));
                }
                if opened != steps + 1 {
                    report(seq, format!("step {opened} follows step {steps}"));
                }
                step = Some(opened);
                steps = opened;
                calls.clear();
            }
            topic::STEP_END => {
                if (turn, step) != (Some(number(event, "turn")), Some(number(event, "step"))) {
                    report(seq, format!("{} closes no open step", event.ty));
                }
                step = None;
            }
            ty => {
                if step.is_none() {
                    report(seq, format!("{ty} is outside any open step"));
                }
                // A record that names where it happened must name the place it
                // is in. One that names nothing is placed by its position.
                for (field, open) in [("turn", turn), ("step", step)] {
                    if let Some(named) = event.data.get(field).and_then(|it| it.as_u64()) {
                        if Some(named) != open {
                            report(seq, format!("{ty} names {field} {named}, not {open:?}"));
                        }
                    }
                }
                if ty == topic::TOOL_CALL {
                    calls.push(text(event, "id"));
                }
                if ty == topic::TOOL_RESULT && !calls.contains(&text(event, "call_id")) {
                    report(
                        seq,
                        "a tool result answers no call of this step".to_string(),
                    );
                }
            }
        }
    }
    if let Some(open) = turn {
        found.push(format!("the journal ends with turn {open} open"));
    }
    found
}

/// The tool the mock model calls, replaced by one that panics, so a case can
/// put a contained failure inside a step.
struct Boom;

#[async_trait::async_trait]
impl Tool for Boom {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "echo".to_string(),
            description: "panics".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        panic!("deliberate: a tool that panics mid-step");
    }
}

/// A provider whose every call fails, so a case can end a turn part way.
fn always_fails(bus: &EventBus) -> EffectHandle {
    bus.on_waterfall::<LlmStream, _>(|_ev, _next| {
        Box::pin(async {
            Err(LlmError::Provider {
                status: 503,
                message: "upstream is down".into(),
                retry_after_ms: None,
                request_id: None,
            })
        })
    })
}

/// Wait until the journal carries a record of type `ty`, so a case acts at a
/// point in the run rather than at a point on the clock.
async fn until_written(h: &Harness, ty: &str) {
    for _ in 0..2_000 {
        if h.engine.log().events().iter().any(|event| event.ty == ty) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("no {ty} record was written");
}

fn journal(h: &Harness) -> Vec<SessionEvent> {
    tetanus_session::replay(&h.log_path).expect("the journal reads back")
}

fn count(journal: &[SessionEvent], ty: &str) -> usize {
    journal.iter().filter(|event| event.ty == ty).count()
}

fn numbers(journal: &[SessionEvent], ty: &str, field: &str) -> Vec<u64> {
    journal
        .iter()
        .filter(|event| event.ty == ty)
        .map(|event| number(event, field))
        .collect()
}

fn number(event: &SessionEvent, field: &str) -> u64 {
    event.data[field].as_u64().unwrap_or_default()
}

fn text(event: &SessionEvent, field: &str) -> String {
    event.data[field].as_str().unwrap_or_default().to_string()
}
