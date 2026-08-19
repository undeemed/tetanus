//! Test Design Specification: upstream resume behaviour, ported.
//!
//! Feature under test: what a session is after a restart. Upstream pins it in
//! `packages/core/agent-loop/tests/resume.spec.ts`; each case names the
//! upstream case it comes from.
//!
//! Approach: a journal root in a temporary directory, one engine per process
//! lifetime, so a resume really is a second store reading a cold journal.
//! Upstream's file is mostly about its agent factory - identity registration,
//! abort signals, transactional setup, publication ordering and rollback -
//! which tetanus does not have; those cases have nothing to restate and stay
//! rows in `docs/parity.md`.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{AgentPromptParams, Engine, SessionCreateParams};
use tetanus_session::SessionLog;
use tetanus_turn::events::AgentRequest;
use tetanus_turn::llm::{ModelRequest, Role};
use tetanus_turn::log::topic;

/// TC-PORT-RESUME-1: a resumed session continues its journal - history, turn
/// numbering, and one contiguous sequence.
///
/// Upstream: "resume reloads a persisted session: history + turn numbering
/// continue, no duplicate seqs". TC-AGENT-6 in `agent.rs` already pins the
/// turn-numbering half from the summary a caller reads back; this case is
/// about the journal itself, which is where "no duplicate seqs" lives.
///
/// Input: one turn, then a second engine over the same root, then a second
/// turn.
/// Expected: turn numbers 1 then 2, `seq` contiguous from 0 across both runs,
/// and every event of the second turn stamped 2.
#[tokio::test]
async fn a_resumed_session_continues_its_journal() {
    let dir = TempDir::new().expect("temp dir");

    let before = engine_over(&dir);
    before
        .session_create(SessionCreateParams {
            session_id: Some("resumed".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    before
        .agent_prompt(prompt("resumed", "first"))
        .await
        .expect("turn");
    let first_run = before
        .sessions()
        .live("resumed")
        .expect("live")
        .log
        .events();
    drop(before);

    let after = engine_over(&dir);
    after
        .session_create(SessionCreateParams {
            session_id: Some("resumed".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("resume");
    after
        .agent_prompt(prompt("resumed", "second"))
        .await
        .expect("turn");

    let events = after.sessions().live("resumed").expect("live").log.events();
    assert!(
        events.len() > first_run.len(),
        "the second turn was appended to the first run's journal"
    );
    for (i, event) in events.iter().enumerate() {
        assert_eq!(
            event.seq, i as u64,
            "seq {i} is not contiguous after a resume"
        );
    }
    assert_eq!(
        &events[..first_run.len()],
        &first_run[..],
        "the first run's events are unchanged by the resume"
    );

    let turns: Vec<u64> = events
        .iter()
        .filter(|e| e.ty == topic::TURN_START)
        .map(|e| e.data["turn"].as_u64().expect("turn"))
        .collect();
    assert_eq!(turns, vec![1, 2], "numbering continues past the seed");
    let second: Vec<u64> = events[first_run.len()..]
        .iter()
        .filter_map(|e| e.data.get("turn").and_then(|v| v.as_u64()))
        .collect();
    assert!(
        second.iter().all(|n| *n == 2),
        "every event of the resumed turn is stamped 2: {second:?}"
    );
}

/// TC-PORT-RESUME-2: the first request after a resume carries the earlier
/// transcript.
///
/// Upstream: "resumes a pre-react-loop session including pre-identity message
/// events" - a resumed session is one the model can continue, not a fresh
/// context that happens to share a file.
///
/// Expected: the first request of the resumed turn opens with the first run's
/// user message and holds its assistant reply, and the new prompt is last.
#[tokio::test]
async fn the_first_request_after_a_resume_carries_the_transcript() {
    let dir = TempDir::new().expect("temp dir");

    let before = engine_over(&dir);
    before
        .session_create(SessionCreateParams {
            session_id: Some("carried".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    before
        .agent_prompt(prompt("carried", "what came before"))
        .await
        .expect("turn");
    drop(before);

    let after = engine_over(&dir);
    after
        .session_create(SessionCreateParams {
            session_id: Some("carried".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("resume");
    let live = after.sessions().live("carried").expect("live");
    let (requests, _record) = record_requests(&live.bus);

    after
        .agent_prompt(prompt("carried", "what comes after"))
        .await
        .expect("turn");

    let requests = requests.lock().expect("requests").clone();
    let first = &requests[0];
    let carried: Vec<(Role, String)> = first
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| (m.role, m.content.clone()))
        .collect();

    assert_eq!(
        carried.first().map(|m| m.1.as_str()),
        Some("what came before"),
        "the resumed request opens with the earlier prompt: {carried:?}"
    );
    assert!(
        carried.iter().any(|(role, _)| *role == Role::Assistant),
        "the earlier reply is history too: {carried:?}"
    );
    assert_eq!(
        carried.last().map(|m| m.1.as_str()),
        Some("what comes after"),
        "and the new prompt is the last thing the model reads"
    );
}

/// TC-PORT-RESUME-3: a journal is crash-repaired once, not once per resume.
///
/// Upstream: "resume cannot crash-repair a turn owned by a live agent" - the
/// same rule from the other end: repair belongs to the transition from cold to
/// live, so a journal that is already whole is left exactly as it is.
///
/// Input: a journal with an open turn, resumed twice.
/// Expected: the first resume appends the closers; the second appends nothing
/// at all, so the closers are not written a second time.
#[tokio::test]
async fn a_journal_is_repaired_once_and_not_once_per_resume() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("cut-off.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"session/start","seq":0,"time":1,"data":{"session_id":"cut-off","provider":"mock","model":"mock-model","max_steps":8}}"#,
            "\n",
            r#"{"type":"turn/start","seq":1,"time":2,"data":{"turn":1}}"#,
            "\n",
            r#"{"type":"step/start","seq":2,"time":3,"data":{"turn":1,"step":1}}"#,
            "\n",
        ),
    )
    .expect("seed");

    let first = engine_over(&dir);
    first
        .session_create(SessionCreateParams {
            session_id: Some("cut-off".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("resume");
    let repaired = first.sessions().live("cut-off").expect("live").log.events();
    drop(first);

    assert_eq!(
        repaired.iter().map(|e| e.ty.as_str()).collect::<Vec<_>>(),
        vec![
            "session/start",
            "turn/start",
            "step/start",
            "step/end",
            "turn/end"
        ],
        "the open turn was closed"
    );

    let second = engine_over(&dir);
    second
        .session_create(SessionCreateParams {
            session_id: Some("cut-off".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("resume again");
    let again = second
        .sessions()
        .live("cut-off")
        .expect("live")
        .log
        .events();

    assert_eq!(again, repaired, "a whole journal is left exactly as it is");
}

/// An engine over one journal root. A new one per case stands for a restart.
fn engine_over(dir: &TempDir) -> HarnessEngine {
    HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    })
}

fn prompt(session_id: &str, content: &str) -> AgentPromptParams {
    AgentPromptParams {
        session_id: session_id.into(),
        content: content.into(),
    }
}

/// Record every model request a session's turns build, in order.
fn record_requests(
    bus: &tetanus_core::EventBus,
) -> (Arc<Mutex<Vec<ModelRequest>>>, tetanus_core::EffectHandle) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let handle = bus.on_waterfall::<AgentRequest, _>(move |ev, next| {
        sink.lock().expect("requests").push(ev.request.clone());
        Box::pin(next.run(ev))
    });
    (seen, handle)
}
