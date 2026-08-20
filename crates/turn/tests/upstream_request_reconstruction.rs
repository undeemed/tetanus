//! Test Design Specification: upstream request reconstruction, ported.
//!
//! Feature under test: the messages the driver dispatches, read as a sequence
//! rather than one at a time. Upstream pins the same rules in
//! `packages/core/agent-loop/tests/request-reconstruction.spec.ts`; each case
//! names the upstream case it comes from.
//!
//! Approach: a real mock turn with the requests recorded on the `agent/request`
//! waterfall, so the assertion is about what the adapter was actually handed.
//! Two properties are stated over that sequence: each request append-extends
//! the one before it, and each request's messages are what the journal alone
//! derives at the moment it went out.
//!
//! Features NOT tested here: the request envelope - model, tools, system text,
//! sampling - rebuilding from the log. Upstream folds a `request/header` event
//! for that; tetanus logs no header, so only the messages half of upstream's
//! theorem has a counterpart. Frozen requests have none either: a tetanus
//! request is an owned value per step, so there is no shared object to freeze.
//!
//! Environmental needs: none. No network, no credential.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod harness;

use std::sync::{Arc, Mutex};

use harness::Harness;
use tetanus_core::{EffectHandle, EventBus};
use tetanus_session::SessionEvent;
use tetanus_turn::events::AgentRequest;
use tetanus_turn::llm::{Message, ModelRequest, Role};
use tetanus_turn::log::{derive_messages, topic};

/// TC-PORT-REQ-1: within one turn, each step's request append-extends the one
/// before it.
///
/// Upstream: `request-reconstruction.spec.ts`, "each step request within a turn
/// append-extends the previous, frozen end to end". Upstream also asserts the
/// request objects are frozen and that one `request/header` was logged; tetanus
/// hands out an owned request per step and logs no header, so the extension is
/// the whole assertion.
///
/// Input: one mock turn - a step that calls a tool, then a step that answers.
/// Expected: two requests, and the first's messages are a prefix of the
/// second's. A step that rewrote or dropped history rather than adding to it
/// would break the prefix, and the model would see a conversation it never had.
#[tokio::test]
async fn each_step_request_append_extends_the_one_before_it() {
    let h = Harness::new("port-req-step-prefix").await;
    let (requests, _record) = record_requests(h.bus());

    h.engine.run_turn("go").await.expect("the turn ran");

    let requests = requests.lock().expect("requests").clone();
    assert_eq!(requests.len(), 2, "one request per step");
    assert_append_extends(&requests[0], &requests[1]);
}

/// TC-PORT-REQ-2: a later turn append-extends the previous turn.
///
/// Upstream: `request-reconstruction.spec.ts`, "a later turn append-extends the
/// previous turn (one conversation, one log)".
///
/// Input: two mock turns on the same engine and the same journal.
/// Expected: the first request of the second turn append-extends the last
/// request of the first. One session is one conversation: a new turn continues
/// the log, it does not start a second one beside it.
#[tokio::test]
async fn a_later_turn_append_extends_the_previous_turn() {
    let h = Harness::new("port-req-turn-prefix").await;
    let (requests, _record) = record_requests(h.bus());

    h.engine.run_turn("first").await.expect("turn one");
    h.engine.run_turn("second").await.expect("turn two");

    let requests = requests.lock().expect("requests").clone();
    assert_eq!(requests.len(), 4, "two steps in each of two turns");
    assert_append_extends(&requests[1], &requests[2]);
}

/// TC-PORT-REQ-3: every request's messages rebuild from the session log alone.
///
/// Upstream: `request-reconstruction.spec.ts`, "THEOREM: every request rebuilds
/// byte-equal from the session log alone". Upstream rebuilds the whole request,
/// envelope included, by folding its `request/header` events; tetanus logs no
/// header, so this restates the messages half. The boundary is the same one
/// upstream uses: the events before that step's first `assistant/chunk` are
/// everything the log held when the request went out.
///
/// Input: two mock turns, then each recorded request checked against a
/// derivation over the journal prefix that preceded it.
/// Expected: for every request, the messages it carried - less the system
/// message, which is assembled and not derived - equal `derive_messages` over
/// that prefix. This is what makes replay honest: a request the log cannot
/// reproduce is a request nobody can audit afterwards.
#[tokio::test]
async fn every_request_rebuilds_from_the_log_alone() {
    let h = Harness::new("port-req-theorem").await;
    let (requests, _record) = record_requests(h.bus());

    h.engine.run_turn("go").await.expect("turn one");
    h.engine.run_turn("again").await.expect("turn two");

    let requests = requests.lock().expect("requests").clone();
    let events = h.engine.log().events();
    let boundaries = first_chunk_of_each_step(&events);
    assert_eq!(
        boundaries.len(),
        requests.len(),
        "one dispatched request per step that streamed"
    );

    for (index, request) in requests.iter().enumerate() {
        let before: Vec<SessionEvent> = events
            .iter()
            .filter(|event| event.seq < boundaries[index])
            .cloned()
            .collect();
        assert_eq!(
            sent_history(request),
            derive_messages(&before),
            "request {index} is the derivation of the log it went out on"
        );
    }
}

/// Assert that `later` is `earlier` with messages added to the end.
fn assert_append_extends(earlier: &ModelRequest, later: &ModelRequest) {
    assert!(
        later.messages.len() > earlier.messages.len(),
        "a later request carries more than the one before it: {} then {}",
        earlier.messages.len(),
        later.messages.len()
    );
    assert_eq!(
        later.messages[..earlier.messages.len()],
        earlier.messages[..],
        "the earlier request is a prefix of the later one"
    );
}

/// The history a request carried, which is everything but the assembled system
/// message. The system prompt is composed per step and never derived, so it is
/// not part of what the log has to reproduce.
fn sent_history(request: &ModelRequest) -> Vec<Message> {
    request
        .messages
        .iter()
        .filter(|message| message.role != Role::System)
        .cloned()
        .collect()
}

/// The `seq` of the first `assistant/chunk` of each step, in step order. A
/// request is dispatched after its step's history is derived and before its
/// first chunk lands, so that seq is the boundary of the log it was built on.
fn first_chunk_of_each_step(events: &[SessionEvent]) -> Vec<u64> {
    let mut seen: Vec<(u64, u64)> = Vec::new();
    let mut boundaries = Vec::new();
    for event in events.iter().filter(|e| e.ty == topic::ASSISTANT_CHUNK) {
        let step = (
            event.data.get("turn").and_then(|t| t.as_u64()).unwrap_or(0),
            event.data.get("step").and_then(|s| s.as_u64()).unwrap_or(0),
        );
        if !seen.contains(&step) {
            seen.push(step);
            boundaries.push(event.seq);
        }
    }
    boundaries
}

/// Record every request the driver builds, in step order.
fn record_requests(bus: &EventBus) -> (Arc<Mutex<Vec<ModelRequest>>>, EffectHandle) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let handle = bus.on_waterfall::<AgentRequest, _>(move |ev, next| {
        let sink = Arc::clone(&sink);
        Box::pin(async move {
            let request = next.run(ev).await;
            sink.lock().expect("requests").push(request.clone());
            request
        })
    });
    (seen, handle)
}
