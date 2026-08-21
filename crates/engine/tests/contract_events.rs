//! Test Design Specification: the durable payloads the contract publishes.
//!
//! Feature under test: `docs/interface-contract.md` section 4.3.1, the table
//! naming what `SessionEvent.data` carries for each of the ten durable types,
//! and the promises stated under it.
//!
//! Approach: run one real turn through the engine, then read the journal back
//! through `session.events` - the same call a surface makes - and assert
//! against what the engine actually wrote. `crates/protocol/tests/wire.rs`
//! TC-PROTO-10 already shows the contract's own example payloads parse; that
//! is the boundary type agreeing with itself. This suite closes the other
//! half, that the engine's output is what the boundary type describes, which
//! is what a payload rename would break silently.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    AgentPromptParams, Engine, SessionCreateParams, SessionEventsParams,
};
use tetanus_protocol::types::{KnownEvent, SessionEvent, TurnSummary};

/// The ten durable types section 4.3.1 tabulates, in the order it tabulates
/// them.
const DOCUMENTED: &[&str] = &[
    "session/start",
    "turn/start",
    "step/start",
    "user/message",
    "assistant/chunk",
    "assistant/message",
    "tool/call",
    "tool/result",
    "step/end",
    "turn/end",
];

/// TC-CONTRACT-1: every documented event a real turn writes parses, and the
/// turn writes all ten.
///
/// Section 4.3.1: `SessionEvent::parse()` returns `KnownEvent` for the
/// documented types. A `None` on one of those means the engine wrote a payload
/// the published boundary type cannot read - a renamed or dropped field -
/// which is a major change made by accident.
///
/// The claim is stated over the documented types rather than over every event
/// the turn wrote, because section 4.3.2 says the durable vocabulary grows: a
/// new type is a free string that a surface passes through, and its
/// `KnownEvent` variant follows later. Asserting that *nothing* is unparsed
/// would make adding a durable type impossible without a boundary change in
/// the same commit, which is the opposite of what that section promises.
/// TC-CONTRACT-1b holds the other half - that an undocumented type is passed
/// through and not dropped.
///
/// Expected: no documented event parses to `None`, and every one of the ten
/// types is present, so the case cannot pass by writing nothing.
#[tokio::test]
async fn every_event_a_turn_writes_is_one_the_contract_describes() {
    let (_engine, events) = one_turn("contract-parses").await;

    let unparsed: Vec<&str> = events
        .iter()
        .filter(|event| DOCUMENTED.contains(&event.ty.as_str()))
        .filter(|event| event.parse().is_none())
        .map(|event| event.ty.as_str())
        .collect();
    assert!(
        unparsed.is_empty(),
        "the engine wrote payloads the contract cannot read: {unparsed:?}"
    );

    for ty in DOCUMENTED {
        assert!(
            events.iter().any(|event| event.ty == *ty),
            "a documented type never appeared, so the case proves nothing: {ty}"
        );
    }
}

/// TC-CONTRACT-1b: a durable type the boundary has not learned still reaches a
/// surface whole.
///
/// Section 4.3.2: the vocabulary grows, `type` stays a free string, and a
/// surface passes an unknown type through rather than dropping it. The engine
/// writes one such type today - `request/context`, the request envelope the
/// token projections anchor on - so the promise is checkable against a real
/// turn rather than against a fixture.
///
/// Expected: the event is on the journal `session.events` serves, it parses to
/// no `KnownEvent`, and its payload arrives intact.
#[tokio::test]
async fn an_undocumented_type_reaches_a_surface_whole() {
    let (_engine, events) = one_turn("contract-grows").await;

    let envelope = events
        .iter()
        .find(|event| event.ty == "request/context")
        .expect("the turn wrote a request envelope");

    assert!(
        envelope.parse().is_none(),
        "a type the boundary has not learned has no KnownEvent variant yet"
    );
    assert!(
        envelope.data.get("provider").is_some_and(|p| p.is_string()),
        "the payload arrived whole: {}",
        envelope.data
    );
    assert!(
        envelope
            .data
            .get("system_tokens")
            .is_some_and(|t| t.is_u64()),
        "the payload arrived whole: {}",
        envelope.data
    );
}

/// TC-CONTRACT-2: a result is paired to its call by id, and the pairing
/// survives a read that starts mid-turn.
///
/// Section 4.3.1: "`tool/result.call_id` is the correlation id, and it equals
/// the `tool/call.id` that asked for it", and the result also cites its call
/// in `sourceEventSeqs`.
///
/// Expected: the ids match through the parsed types, not the raw JSON, and the
/// citation names the seq of that same `tool/call`.
#[tokio::test]
async fn a_tool_result_names_the_call_it_answers() {
    let (_engine, events) = one_turn("contract-pairing").await;

    let call = events
        .iter()
        .find(|event| event.ty == "tool/call")
        .expect("the mock turn calls a tool");
    let result = events
        .iter()
        .find(|event| event.ty == "tool/result")
        .expect("and it answers");

    let Some(KnownEvent::ToolCall { id, .. }) = call.parse() else {
        panic!("tool/call did not parse: {:?}", call.data);
    };
    let Some(KnownEvent::ToolResult { call_id, name, .. }) = result.parse() else {
        panic!("tool/result did not parse: {:?}", result.data);
    };

    assert_eq!(call_id, id, "the correlation id is the call's own id");
    assert!(!name.is_empty(), "a result names its tool");
    assert_eq!(
        result.source_event_seqs.as_deref(),
        Some(&[call.seq][..]),
        "and cites the call, so a mid-turn reader can still pair them"
    );
}

/// TC-CONTRACT-3: the summary restates the last assistant message, it does not
/// invent a second answer.
///
/// Section 4.3.1: "The turn's answer is the last `assistant/message.content` -
/// `TurnSummary.content` is that same text, restated for a caller that did not
/// stream. A surface reads one or the other, never both, or it renders the
/// answer twice."
///
/// Expected: the two are equal, and `turn/end` does not carry the answer
/// itself, which is what makes reading both a duplication rather than a
/// disagreement.
#[tokio::test]
async fn the_summary_restates_the_last_assistant_message() {
    let (summary, events) = one_turn_summary("contract-answer").await;

    let last = events
        .iter()
        .filter(|event| event.ty == "assistant/message")
        .filter_map(|event| match event.parse() {
            Some(KnownEvent::AssistantMessage { content, .. }) => Some(content),
            _ => None,
        })
        .next_back()
        .expect("the turn answered");

    assert!(!last.is_empty(), "the last message is the answer");
    assert_eq!(summary.content, last);

    let end = events
        .iter()
        .find(|event| event.ty == "turn/end")
        .expect("the turn closed");
    assert!(
        end.data.get("content").is_none(),
        "turn/end deliberately does not repeat the answer: {:?}",
        end.data
    );
}

/// TC-CONTRACT-4: a chunk says which turn and step it belongs to.
///
/// Section 4.3.1 gives `assistant/chunk` its chunk shape "plus `turn` and
/// `step`". Without them a surface has to track boundaries itself to place a
/// delta, which is the work the boundary exists to remove.
///
/// Expected: every chunk parses with a turn and step that name a step the
/// journal actually opened.
#[tokio::test]
async fn every_chunk_says_which_step_it_belongs_to() {
    let (_engine, events) = one_turn("contract-chunk-placement").await;

    let opened: Vec<(u64, u32)> = events
        .iter()
        .filter_map(|event| match event.parse() {
            Some(KnownEvent::StepStart { turn, step }) => Some((turn, step)),
            _ => None,
        })
        .collect();
    assert_eq!(opened.len(), 2, "the mock turn opens two steps");

    let placed: Vec<(u64, u32)> = events
        .iter()
        .filter(|event| event.ty == "assistant/chunk")
        .map(|event| match event.parse() {
            Some(KnownEvent::AssistantChunk { turn, step, .. }) => (turn, step),
            _ => panic!("a chunk did not parse: {:?}", event.data),
        })
        .collect();
    assert!(!placed.is_empty(), "the turn streamed something");
    for at in &placed {
        assert!(
            opened.contains(at),
            "a chunk claims turn {} step {}, which never opened: {opened:?}",
            at.0,
            at.1
        );
    }
}

/// One mock turn, and the journal it left, read back the way a surface reads
/// it. The engine is returned so the temporary root outlives the call.
async fn one_turn(name: &str) -> (Held, Vec<SessionEvent>) {
    let (_summary, events, held) = run(name).await;
    (held, events)
}

async fn one_turn_summary(name: &str) -> (TurnSummary, Vec<SessionEvent>) {
    let (summary, events, _held) = run(name).await;
    (summary, events)
}

/// The engine and its journal root, kept alive for the length of a case.
struct Held {
    _engine: HarnessEngine,
    _dir: TempDir,
}

async fn run(name: &str) -> (TurnSummary, Vec<SessionEvent>, Held) {
    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });
    let info = engine
        .session_create(SessionCreateParams {
            session_id: Some(name.into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    let summary = engine
        .agent_prompt(AgentPromptParams {
            session_id: info.session_id.clone(),
            content: "say hello and echo it".into(),
        })
        .await
        .expect("prompt")
        .summary;
    let page = engine
        .session_events(SessionEventsParams {
            session_id: info.session_id,
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("events");
    assert!(page.eof, "the whole journal fits one page");
    (
        summary,
        page.events,
        Held {
            _engine: engine,
            _dir: dir,
        },
    )
}

/// TC-CONTRACT-5: a pushed event is byte-identical to the line on disk.
///
/// The contract says this twice - section 4.3 calls `SessionEvent` "byte-
/// identical to one line of the JSONL journal", and section 4.7 says dropping
/// the push envelope "makes the stream byte-identical to the journal on disk"
/// - and until now nothing checked it anywhere.
///
/// Section 7.6 said TC-PROTO-5 did. It cannot: that case lives in
/// `crates/protocol`, which deliberately does not depend on the crate owning
/// the journal type, so it compares the wire type against a hand-written
/// literal. A literal is a copy of the journal's shape and goes stale exactly
/// when the shape changes, which is the moment the check was for. It also
/// compares through `serde_json::to_value`, which is a structural comparison
/// and blind to field order - so even "identical" was being read loosely.
///
/// This is the case that can make the claim, because `crates/engine` is where
/// both types exist. It compares the two serialized *strings*, so a reordered
/// field, a changed rename, or a differing `skip_serializing_if` all fail here
/// while every existing suite stays green.
#[tokio::test]
async fn a_pushed_event_is_byte_identical_to_the_line_on_disk() {
    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });

    let info = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("session.create");
    engine
        .agent_prompt(AgentPromptParams {
            session_id: info.session_id.clone(),
            content: "write a line to the journal".into(),
        })
        .await
        .expect("agent.prompt");

    // What the journal holds, through the crate that owns it.
    let on_disk =
        tetanus_session::replay(std::path::Path::new(&info.path)).expect("the journal replays");
    assert!(!on_disk.is_empty(), "a turn wrote something");

    // What the boundary carries, through the call a surface makes.
    let served = engine
        .session_events(SessionEventsParams {
            session_id: info.session_id,
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("session.events")
        .events;

    assert_eq!(
        served.len(),
        on_disk.len(),
        "the boundary serves every line the journal holds"
    );

    for (wire, line) in served.iter().zip(&on_disk) {
        let wire_json = serde_json::to_string(wire).expect("serialize the wire event");
        let line_json = serde_json::to_string(line).expect("serialize the journal event");
        assert_eq!(
            wire_json, line_json,
            "byte-identical is a claim about bytes: `{}` differs from the line at seq {}",
            wire.ty, line.seq
        );
    }

    // And at least one event carries `sourceEventSeqs`, so the case covers the
    // field whose rename and omission are the likeliest thing to drift.
    assert!(
        on_disk.iter().any(|e| e.source_event_seqs.is_some()),
        "a turn writes at least one surface event, which is where the camel-case field lives"
    );
}
