//! Contract conformance: the wire shapes are what the document says they are.
//!
//! Test design: every case here fixes one clause of `docs/interface-contract.md`
//! that a refactor could break silently. Expected results are literal JSON, not
//! round trips through the same code that produced them.

use serde_json::json;
use tetanus_protocol::methods::{AgentStatusPush, SessionEventPush};
use tetanus_protocol::rpc::{ErrorCode, Id, Message, Payload, Response, RpcError, V2};
use tetanus_protocol::types::{
    AgentState, Chunk, KnownEvent, SessionEvent, StopReason, TurnSummary, Usage,
};
use tetanus_protocol::{is_compatible, PROTOCOL_VERSION};

/// TC-PROTO-1: a request, a response and a notification demultiplex to the
/// right `Message` variant, and no other.
#[test]
fn frames_demultiplex_by_shape() {
    let request = json!({"jsonrpc":"2.0","id":1,"method":"session.list","params":{}});
    let response = json!({"jsonrpc":"2.0","id":1,"result":{"sessions":[]}});
    let notification = json!({"jsonrpc":"2.0","method":"agent/status","params":{}});

    assert!(matches!(
        serde_json::from_value::<Message>(request).unwrap(),
        Message::Request(_)
    ));
    assert!(matches!(
        serde_json::from_value::<Message>(response).unwrap(),
        Message::Response(_)
    ));
    assert!(matches!(
        serde_json::from_value::<Message>(notification).unwrap(),
        Message::Notification(_)
    ));
}

/// TC-PROTO-2: the envelope tag is checked, not assumed.
#[test]
fn a_frame_without_the_2_0_tag_is_rejected() {
    let frame = json!({"jsonrpc":"1.0","id":1,"method":"session.list"});
    assert!(serde_json::from_value::<Message>(frame).is_err());
}

/// TC-PROTO-3: an error response carries `error` and never `result`, and its
/// code round-trips to the named variant.
#[test]
fn an_error_response_is_shaped_as_documented() {
    let response = Response {
        jsonrpc: V2,
        id: Id::Number(7),
        payload: Payload::Error(
            RpcError::new(ErrorCode::SessionNotFound, "no session `abc`")
                .with_data(json!({"session_id":"abc"})),
        ),
    };
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "error": {
                "code": -32002,
                "message": "no session `abc`",
                "data": {"session_id": "abc"},
            },
        })
    );
    let Payload::Error(error) = response.payload else {
        unreachable!("built as an error")
    };
    assert_eq!(error.kind(), Some(ErrorCode::SessionNotFound));
}

/// TC-PROTO-4: an unknown code is surfaced, not remapped onto a known one.
#[test]
fn an_unknown_error_code_stays_unknown() {
    let error: RpcError = serde_json::from_value(json!({"code": -32050, "message": "later"}))
        .expect("a well-formed error object with an unknown code still parses");
    assert_eq!(error.kind(), None);
    assert_eq!(error.code, -32050);
}

/// TC-PROTO-5: a durable event keeps the journal's own field names, including
/// the camel-case `sourceEventSeqs` and the omission of it when absent.
#[test]
fn a_session_event_matches_the_journal_line() {
    let event = SessionEvent {
        ty: "assistant/message".into(),
        seq: 7,
        time: 1_755_558_000_123,
        data: json!({"content": "hi"}),
        source_event_seqs: Some(vec![3, 4]),
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "assistant/message",
            "seq": 7,
            "time": 1_755_558_000_123u64,
            "data": {"content": "hi"},
            "sourceEventSeqs": [3, 4],
        })
    );

    let bare = SessionEvent {
        source_event_seqs: None,
        ..event
    };
    let bare = serde_json::to_value(&bare).unwrap();
    assert!(bare.get("sourceEventSeqs").is_none());
}

/// TC-PROTO-6: a growable enum accepts a variant this build does not know, so
/// a minor-version addition never breaks an older surface.
#[test]
fn unknown_enum_variants_survive_a_round_trip() {
    let state: AgentState = serde_json::from_value(json!("compacting")).unwrap();
    assert_eq!(state, AgentState::Other("compacting".into()));
    assert_eq!(serde_json::to_value(&state).unwrap(), json!("compacting"));

    let reason: StopReason = serde_json::from_value(json!("budget-exhausted")).unwrap();
    assert_eq!(reason, StopReason::Other("budget-exhausted".into()));

    assert_eq!(
        serde_json::to_value(AgentState::Running).unwrap(),
        json!("running")
    );
    assert_eq!(
        serde_json::to_value(StopReason::PreStepRejected).unwrap(),
        json!("pre-step-rejected")
    );
}

/// TC-PROTO-7: both pushes name their session, so one connection can carry
/// several.
#[test]
fn pushes_carry_the_session_they_belong_to() {
    let event = serde_json::to_value(SessionEventPush {
        session_id: "s1".into(),
        event: SessionEvent {
            ty: "turn/start".into(),
            seq: 0,
            time: 1,
            data: json!({"turn": 1}),
            source_event_seqs: None,
        },
    })
    .unwrap();
    assert_eq!(event["session_id"], json!("s1"));
    assert_eq!(event["event"]["type"], json!("turn/start"));

    let status = serde_json::to_value(AgentStatusPush {
        session_id: "s1".into(),
        state: AgentState::Running,
        turn: Some(1),
        step: Some(2),
    })
    .unwrap();
    assert_eq!(
        status,
        json!({"session_id":"s1","state":"running","turn":1,"step":2})
    );
}

/// TC-PROTO-8: compatibility is decided by the major component alone.
#[test]
fn compatibility_ignores_the_minor_version() {
    assert!(is_compatible(PROTOCOL_VERSION));
    assert!(is_compatible("1.7"));
    assert!(!is_compatible("2.0"));
    assert!(!is_compatible("one"));
}

/// TC-PROTO-9: exit statuses are the contract's, so no surface invents one.
#[test]
fn every_code_maps_to_the_documented_exit_status() {
    assert_eq!(ErrorCode::InvalidParams.exit_status(), 2);
    assert_eq!(ErrorCode::NotImplemented.exit_status(), 3);
    assert_eq!(ErrorCode::SessionNotFound.exit_status(), 4);
    assert_eq!(ErrorCode::MissingCredential.exit_status(), 5);
    assert_eq!(ErrorCode::ProviderError.exit_status(), 6);
    assert_eq!(ErrorCode::Cancelled.exit_status(), 130);
    assert_eq!(ErrorCode::Internal.exit_status(), 1);
}

/// TC-PROTO-10: section 4.3.1. Each durable type parses from the payload the
/// journal actually holds, asserted from literal JSON so the case fails if the
/// engine's payload and the contract's table ever disagree.
#[test]
fn known_payloads_parse_from_the_journal_shape() {
    let cases = vec![
        (
            "session/start",
            json!({ "session_id": "s1", "provider": "mock", "model": "m", "max_steps": 8 }),
        ),
        ("turn/start", json!({ "turn": 1 })),
        ("step/start", json!({ "turn": 1, "step": 1 })),
        ("user/message", json!({ "content": "hi" })),
        (
            "assistant/chunk",
            json!({ "chunk": "text", "delta": "he", "turn": 1, "step": 1 }),
        ),
        (
            "assistant/message",
            json!({
                "content": "hello",
                "reasoning": "",
                "tool_calls": [],
                "finish_reason": "stop",
                "usage": { "prompt_tokens": 3, "completion_tokens": 4 }
            }),
        ),
        (
            "tool/call",
            json!({ "id": "c1", "name": "echo", "arguments": { "text": "hi" } }),
        ),
        (
            "tool/result",
            json!({ "call_id": "c1", "name": "echo", "ok": true, "content": "hi" }),
        ),
        ("step/end", json!({ "turn": 1, "step": 1 })),
        (
            "turn/end",
            json!({ "turn": 1, "steps": 1, "stop_reason": "natural", "stop_veto": null }),
        ),
    ];

    for (ty, data) in cases {
        let event = SessionEvent {
            ty: ty.to_string(),
            seq: 0,
            time: 0,
            data,
            source_event_seqs: None,
        };
        assert!(
            event.parse().is_some(),
            "`{ty}` must parse into a KnownEvent"
        );
    }
}

/// TC-PROTO-11: parsing is a fast path, not a closed vocabulary. An unknown
/// type gives `None`, and the caller still holds the whole raw event.
#[test]
fn an_unknown_type_parses_to_none_and_keeps_its_data() {
    let event = SessionEvent {
        ty: "something/new".into(),
        seq: 4,
        time: 1,
        data: json!({ "whatever": true }),
        source_event_seqs: None,
    };
    assert!(event.parse().is_none());
    assert_eq!(event.data["whatever"], json!(true));
}

/// TC-PROTO-12: a tool result names the call it answers, so a surface pairs
/// them by id and never by arrival order.
#[test]
fn a_tool_result_carries_the_id_of_its_call() {
    let call = SessionEvent {
        ty: "tool/call".into(),
        seq: 7,
        time: 0,
        data: json!({ "id": "call-2", "name": "echo", "arguments": {} }),
        source_event_seqs: None,
    };
    let result = SessionEvent {
        ty: "tool/result".into(),
        seq: 9,
        time: 0,
        data: json!({ "call_id": "call-2", "name": "echo", "ok": false, "content": "no" }),
        source_event_seqs: Some(vec![7]),
    };

    let Some(KnownEvent::ToolCall { id, .. }) = call.parse() else {
        panic!("a tool/call must parse");
    };
    let Some(KnownEvent::ToolResult { call_id, ok, .. }) = result.parse() else {
        panic!("a tool/result must parse");
    };
    assert_eq!(call_id, id);
    assert!(!ok, "a refused call is still a result, not an error");
}

/// TC-PROTO-13: a chunk keeps its variant tag, so a surface can tell visible
/// text from thinking text without inspecting field names.
#[test]
fn a_chunk_keeps_its_variant() {
    let reasoning = SessionEvent {
        ty: "assistant/chunk".into(),
        seq: 2,
        time: 0,
        data: json!({ "chunk": "reasoning", "delta": "hmm", "turn": 1, "step": 1 }),
        source_event_seqs: None,
    };
    let Some(KnownEvent::AssistantChunk { chunk, .. }) = reasoning.parse() else {
        panic!("a chunk must parse");
    };
    assert_eq!(
        chunk,
        Chunk::Reasoning {
            delta: "hmm".into()
        }
    );
}

/// TC-PROTO-14: the fields reserved for the presentation lane are optional on
/// the wire, so a build that does not measure them omits them rather than
/// reporting a zero a surface would render as a fact.
#[test]
fn unmeasured_facts_are_absent_not_zero() {
    let summary = TurnSummary {
        turn: 1,
        steps: 1,
        stop_reason: StopReason::Natural,
        stop_veto: None,
        content: "done".into(),
        duration_ms: None,
        usage: None,
    };
    let json = serde_json::to_value(&summary).expect("serialize");
    assert!(json.get("duration_ms").is_none());
    assert!(json.get("usage").is_none());

    let measured = TurnSummary {
        duration_ms: Some(120),
        usage: Some(Usage {
            prompt_tokens: 3,
            completion_tokens: 4,
        }),
        ..summary
    };
    let json = serde_json::to_value(&measured).expect("serialize");
    assert_eq!(json["usage"]["prompt_tokens"], json!(3));
}

/// TC-PROTO-15: contract section 4.1. A server that cannot read an id still
/// answers, with `id: null`. The value round trips both ways and stays
/// distinct from a numeric or textual id, so a client can tell an answer to
/// its call from an answer to a frame the server could not correlate.
#[test]
fn a_frame_the_server_cannot_correlate_is_answered_with_a_null_id() {
    let refusal = Response {
        jsonrpc: V2,
        id: Id::Null,
        payload: Payload::Error(RpcError::new(ErrorCode::ParseError, "not JSON")),
    };

    let json = serde_json::to_value(&refusal).expect("serialize");
    assert_eq!(json["id"], json!(null), "JSON-RPC 2.0 requires null here");
    assert_eq!(json["error"]["code"], json!(-32700));

    let read: Response = serde_json::from_value(json).expect("deserialize");
    assert_eq!(read, refusal, "the id survives the round trip");

    assert_ne!(Id::Null, Id::Number(0), "null is not id zero");
    assert_ne!(Id::Null, Id::Text(String::new()), "null is not an empty id");

    // A frame is still demultiplexed by shape, null id and all.
    let frame = json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"batch"}});
    assert!(matches!(
        serde_json::from_value::<Message>(frame).expect("demultiplex"),
        Message::Response(_)
    ));
}

/// TC-PROTO-16: contract section 4.3.2. A durable type this version stages -
/// `llm/retry` and `llm/retry-started` - carries every key the section fixes,
/// and still parses to `None`, so a surface renders it raw rather than
/// matching a `KnownEvent` variant that does not exist yet.
#[test]
fn a_staged_type_parses_to_none_and_keeps_every_documented_key() {
    let retry = SessionEvent {
        ty: "llm/retry".into(),
        seq: 11,
        time: 0,
        data: json!({
            "turn": 1,
            "step": 2,
            "provider": "deepseek-official",
            "code": "RATE_LIMIT",
            "message": "429 slow down",
            "retry": 1,
            "max_retries": 2,
            "delay_ms": 500,
        }),
        source_event_seqs: None,
    };
    let started = SessionEvent {
        ty: "llm/retry-started".into(),
        seq: 12,
        time: 0,
        data: json!({ "turn": 1, "step": 2, "retry": 1 }),
        source_event_seqs: None,
    };

    for (event, keys) in [
        (
            &retry,
            vec![
                "turn",
                "step",
                "provider",
                "code",
                "message",
                "retry",
                "max_retries",
                "delay_ms",
            ],
        ),
        (&started, vec!["turn", "step", "retry"]),
    ] {
        assert!(
            event.parse().is_none(),
            "`{}` is staged, not a KnownEvent variant",
            event.ty
        );
        for key in keys {
            assert!(
                event.data.get(key).is_some(),
                "`{}` must carry `{key}`",
                event.ty
            );
        }
    }

    // An unbounded policy has no ceiling to report, and says so rather than
    // reporting a number a reader would take for a limit.
    let unbounded = SessionEvent {
        data: json!({ "max_retries": null }),
        ..retry
    };
    assert_eq!(unbounded.data["max_retries"], json!(null));
}
