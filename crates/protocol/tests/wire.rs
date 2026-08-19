//! Contract conformance: the wire shapes are what the document says they are.
//!
//! Test design: every case here fixes one clause of `docs/interface-contract.md`
//! that a refactor could break silently. Expected results are literal JSON, not
//! round trips through the same code that produced them.

use serde_json::json;
use tetanus_protocol::methods::{AgentStatusPush, SessionEventPush};
use tetanus_protocol::rpc::{ErrorCode, Id, Message, Payload, Response, RpcError, V2};
use tetanus_protocol::types::{AgentState, SessionEvent, StopReason};
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
