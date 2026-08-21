//! Contract conformance: the wire shapes are what the document says they are.
//!
//! Test design: every case here fixes one clause of `docs/interface-contract.md`
//! that a refactor could break silently. Expected results are literal JSON, not
//! round trips through the same code that produced them.

use serde_json::json;
use tetanus_protocol::methods::{
    capability, method, push, AgentStatusPush, ApprovalSetParams, ApproveParams, ApproveResult,
    SessionEventPush, SessionForkParams,
};
use tetanus_protocol::rpc::{ErrorCode, Id, Message, Payload, Response, RpcError, V2};
use tetanus_protocol::types::{
    AgentState, ApprovalOutcome, ApprovalPolicy, Chunk, ConfigEntry, ConfigLayer, KnownEvent,
    SessionEvent, StopReason, TurnSummary, Usage, REDACTED,
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
        (
            "session/start",
            json!({
                "session_id": "s2", "provider": "mock", "model": "m", "max_steps": 8,
                "parent_session": "s1", "fork_seq": 6
            }),
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

/// TC-PROTO-17: contract section 4.3. A value the engine withholds is spelled
/// the one way, so a surface that wants to mark the entry recognises it, and it
/// travels as an ordinary string, so a surface that does nothing new still
/// renders a whole dump. The entry keeps its key and its layer: what is
/// withheld is the value, not the fact that the key is set.
#[test]
fn a_withheld_value_is_an_ordinary_value() {
    let entry = ConfigEntry {
        key: "llm.providers.deepseek.api_key".into(),
        value: json!(REDACTED),
        layer: ConfigLayer::File,
    };

    let wire = serde_json::to_value(&entry).expect("serialize");
    assert_eq!(wire["key"], json!("llm.providers.deepseek.api_key"));
    assert_eq!(wire["value"], json!("<redacted>"));
    assert_eq!(wire["layer"], json!("file"));
    assert_eq!(
        serde_json::from_value::<ConfigEntry>(wire).expect("parse"),
        entry
    );
}

/// TC-PROTO-18: contract section 4.4.6. `session.fork` names the source and
/// nothing else in its smallest form: both other fields are optional, and an
/// omitted one is absent from the wire rather than sent as `null`, so a server
/// cannot tell "omitted" from "explicitly nothing" and never has to.
///
/// The boundary is `through_seq` and not `from_seq` on purpose: section 4.4.5
/// spends `from_seq` on the *first* event a caller receives, and this is the
/// last event a child keeps. Two names for opposite ends of a range is the
/// cheapest defect a contract can ship.
#[test]
fn fork_params_name_a_source_and_leave_the_rest_optional() {
    let minimal = SessionForkParams {
        session_id: "parent".into(),
        through_seq: None,
        child_session_id: None,
    };

    let wire = serde_json::to_value(&minimal).expect("serialize");
    assert_eq!(wire, json!({ "session_id": "parent" }));
    assert_eq!(
        serde_json::from_value::<SessionForkParams>(wire).expect("parse"),
        minimal
    );

    let full = json!({
        "session_id": "parent",
        "through_seq": 6,
        "child_session_id": "child"
    });
    assert_eq!(
        serde_json::from_value::<SessionForkParams>(full).expect("parse"),
        SessionForkParams {
            session_id: "parent".into(),
            through_seq: Some(6),
            child_session_id: Some("child".into()),
        }
    );

    // The capability a surface checks before it offers the affordance is the
    // method's own name, as every other optional call's is.
    assert_eq!(capability::SESSION_FORK, method::SESSION_FORK);
}

/// TC-PROTO-19: contract section 4.3.1. Lineage is optional on `session/start`
/// in both directions.
///
/// A journal written before forking existed carries neither field and still
/// parses, with lineage read as absent rather than as a default id. A forked
/// journal carries both, and `fork_seq` is a seq, so a child that inherited a
/// parent's whole history reports the parent's last seq and not a count.
#[test]
fn session_start_lineage_is_optional_and_absent_means_no_parent() {
    let opened = SessionEvent {
        ty: "session/start".into(),
        seq: 0,
        time: 1,
        data: json!({ "session_id": "s1", "provider": "mock", "model": "m", "max_steps": 8 }),
        source_event_seqs: None,
    };
    assert_eq!(
        opened.parse().expect("parse"),
        KnownEvent::SessionStart {
            session_id: "s1".into(),
            provider: "mock".into(),
            model: "m".into(),
            max_steps: 8,
            parent_session: None,
            fork_seq: None,
        }
    );

    let forked = SessionEvent {
        data: json!({
            "session_id": "s2", "provider": "mock", "model": "m", "max_steps": 8,
            "parent_session": "s1", "fork_seq": 6
        }),
        ..opened
    };
    assert_eq!(
        forked.parse().expect("parse"),
        KnownEvent::SessionStart {
            session_id: "s2".into(),
            provider: "mock".into(),
            model: "m".into(),
            max_steps: 8,
            parent_session: Some("s1".into()),
            fork_seq: Some(6),
        }
    );

    // What the engine writes is what a reader gets back: an absent parent is
    // not serialized as `null`, so a journal line stays the shape section
    // 4.3.1's table lists.
    let round_tripped = serde_json::to_value(
        serde_json::from_value::<KnownEvent>(json!({
            "type": "session/start",
            "session_id": "s1", "provider": "mock", "model": "m", "max_steps": 8
        }))
        .expect("parse"),
    )
    .expect("serialize");
    assert_eq!(round_tripped.get("parent_session"), None);
    assert_eq!(round_tripped.get("fork_seq"), None);
}

/// TC-PROTO-20: contract section 4.4.7. An approval question names the audit
/// line it was written as, the tool it is about, and the call it decides.
///
/// `request_id` is the `approval/asked.id` and `call_id` the `tool/call.id`, so
/// a surface can attach one prompt to both the journal and the call it already
/// streamed. Both optional fields are absent rather than `null` when the asker
/// had none, which is what lets a surface tell "no call" from "a call named
/// null".
#[test]
fn an_approve_request_names_its_audit_line_its_tool_and_its_call() {
    let params = ApproveParams {
        session_id: "s1".into(),
        request_id: "ask-1".into(),
        tool_name: "shell".into(),
        call_id: Some("call-7".into()),
        reason: Some("writes outside the workspace".into()),
    };

    let wire = serde_json::to_value(&params).expect("serialize");
    assert_eq!(
        wire,
        json!({
            "session_id": "s1",
            "request_id": "ask-1",
            "tool_name": "shell",
            "call_id": "call-7",
            "reason": "writes outside the workspace",
        })
    );
    assert_eq!(
        serde_json::from_value::<ApproveParams>(wire).expect("parse"),
        params
    );

    let bare = ApproveParams {
        call_id: None,
        reason: None,
        ..params
    };
    let wire = serde_json::to_value(&bare).expect("serialize");
    assert_eq!(wire.get("call_id"), None);
    assert_eq!(wire.get("reason"), None);

    // The capability a surface checks is the frame's own name with the
    // separator every other capability uses.
    assert_eq!(capability::UI_APPROVE, "ui.approve");
    assert_eq!(push::UI_APPROVE, "ui/approve");
}

/// TC-PROTO-21: contract section 4.4.7. Four outcomes, spelled as the section
/// spells them, and exactly one of them grants.
///
/// `grants()` is the fail-closed rule as one function: a caller that matched
/// the enum itself could forget an arm, and forgetting the wrong one opens a
/// gate.
#[test]
fn one_outcome_grants_and_the_rest_deny() {
    for (outcome, word, grants) in [
        (ApprovalOutcome::AllowedOnce, "allowed-once", true),
        (ApprovalOutcome::Rejected, "rejected", false),
        (ApprovalOutcome::Cancelled, "cancelled", false),
        (ApprovalOutcome::Unavailable, "unavailable", false),
    ] {
        assert_eq!(
            serde_json::to_value(&outcome).expect("serialize"),
            json!(word)
        );
        assert_eq!(
            serde_json::from_value::<ApprovalOutcome>(json!(word)).expect("parse"),
            outcome
        );
        assert_eq!(outcome.grants(), grants, "`{word}` grants: {grants}");
    }

    let result = ApproveResult {
        outcome: ApprovalOutcome::AllowedOnce,
    };
    assert_eq!(
        serde_json::to_value(&result).expect("serialize"),
        json!({ "outcome": "allowed-once" })
    );
}

/// TC-PROTO-22: contract section 4.4.7, and section 4.3's rule for a fallback
/// the engine reads rather than renders.
///
/// A client that answers with a word this build does not know is not a parse
/// failure - section 7.5's fallback is what keeps an added variant minor - and
/// it is not a grant either. Both halves matter: dropping the fallback would
/// break an older engine against a newer surface, and letting it grant would
/// turn any typo into permission.
#[test]
fn an_unknown_outcome_parses_and_denies() {
    let answered = serde_json::from_value::<ApproveResult>(json!({ "outcome": "allowed-always" }))
        .expect("an unknown outcome parses rather than failing the frame");
    assert_eq!(
        answered.outcome,
        ApprovalOutcome::Other("allowed-always".into())
    );
    assert!(
        !answered.outcome.grants(),
        "a word the engine cannot interpret is not a grant"
    );

    // It travels back out as the word it arrived as, so a transcript records
    // what the client actually said.
    assert_eq!(
        serde_json::to_value(&answered).expect("serialize"),
        json!({ "outcome": "allowed-always" })
    );
}

/// TC-PROTO-23: contract section 4.4.7. Two policies, and a third word that
/// stays readable.
///
/// The fallback exists here for the same compatibility reason as everywhere
/// else, but it is never acted on: the engine answers `InvalidParams` naming
/// `policy`, because a caller setting a policy could have named one of the two.
#[test]
fn the_two_policies_round_trip_and_a_third_word_survives() {
    for (policy, word) in [
        (ApprovalPolicy::Ask, "ask"),
        (ApprovalPolicy::Never, "never"),
    ] {
        assert_eq!(
            serde_json::to_value(&policy).expect("serialize"),
            json!(word)
        );
        assert_eq!(
            serde_json::from_value::<ApprovalPolicy>(json!(word)).expect("parse"),
            policy
        );
    }

    let params = ApprovalSetParams {
        session_id: "s1".into(),
        policy: ApprovalPolicy::Never,
    };
    assert_eq!(
        serde_json::to_value(&params).expect("serialize"),
        json!({ "session_id": "s1", "policy": "never" })
    );

    let unknown = serde_json::from_value::<ApprovalSetParams>(
        json!({ "session_id": "s1", "policy": "yolo" }),
    )
    .expect("an unknown policy reaches the engine as a value, not as a parse failure");
    assert_eq!(unknown.policy, ApprovalPolicy::Other("yolo".into()));

    assert_eq!(capability::APPROVAL_SET, method::APPROVAL_SET);
}

/// TC-PROTO-24: contract section 4.3.2. The three `approval/*` types stage
/// exactly as `llm/retry` does: every documented key is carried, and `parse()`
/// still answers `None`, so a surface renders them raw until the version that
/// gives them a `KnownEvent` variant.
#[test]
fn the_approval_audit_types_stage_like_the_others() {
    let asked = SessionEvent {
        ty: "approval/asked".into(),
        seq: 5,
        time: 0,
        data: json!({
            "id": "ask-1",
            "tool_name": "shell",
            "call_id": "call-7",
            "reason": "writes outside the workspace",
        }),
        source_event_seqs: None,
    };
    let decided = SessionEvent {
        ty: "approval/decided".into(),
        seq: 6,
        time: 0,
        data: json!({ "id": "ask-1", "outcome": "rejected" }),
        source_event_seqs: None,
    };
    let policy = SessionEvent {
        ty: "approval/policy".into(),
        seq: 7,
        time: 0,
        data: json!({ "policy": "never" }),
        source_event_seqs: None,
    };

    for (event, keys) in [
        (&asked, vec!["id", "tool_name"]),
        (&decided, vec!["id", "outcome"]),
        (&policy, vec!["policy"]),
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

    // The pair is one to one and shares an id, which is what makes the audit
    // readable at all: the decision is found by the id, never by adjacency.
    assert_eq!(asked.data["id"], decided.data["id"]);

    // The two vocabularies on the journal are the wire enums' own words, so a
    // surface that folds the log and one that reads a frame agree.
    assert_eq!(
        serde_json::from_value::<ApprovalOutcome>(decided.data["outcome"].clone()).expect("parse"),
        ApprovalOutcome::Rejected
    );
    assert_eq!(
        serde_json::from_value::<ApprovalPolicy>(policy.data["policy"].clone()).expect("parse"),
        ApprovalPolicy::Never
    );

    // A question the asker had no call for omits the optional keys rather than
    // carrying them as null.
    let bare = SessionEvent {
        data: json!({ "id": "ask-2", "tool_name": "shell" }),
        ..asked
    };
    assert_eq!(bare.data.get("call_id"), None);
    assert_eq!(bare.data.get("reason"), None);
}

/// TC-PROTO-105: contract section 4.3.3. A journal with no `format` reads as
/// the first one.
///
/// Every journal written before this field existed lacks it, and none of them
/// may become unreadable for that. Absent-means-one is what makes the field
/// addable at all - the alternative is a flag day for data already on disk.
#[test]
fn a_journal_with_no_format_reads_as_the_first_one() {
    let existing = SessionEvent {
        ty: "session/start".into(),
        seq: 0,
        time: 1,
        data: json!({ "session_id": "s1", "provider": "mock", "model": "m", "max_steps": 8 }),
        source_event_seqs: None,
    };
    assert!(
        existing.parse().is_some(),
        "a journal written before this still parses"
    );
    assert_eq!(
        existing.data.get("format"),
        None,
        "and absent is how it says format 1, rather than carrying a 1 nobody wrote"
    );

    let versioned = SessionEvent {
        data: json!({
            "session_id": "s1", "provider": "mock", "model": "m", "max_steps": 8,
            "format": 1
        }),
        ..existing.clone()
    };
    assert!(versioned.parse().is_some());
    assert_eq!(versioned.data["format"], json!(1));

    // A format from a later build is readable *as a value* - which is what
    // lets a reader refuse it deliberately rather than fail to parse the line
    // and report damage instead.
    let newer = SessionEvent {
        data: json!({
            "session_id": "s1", "provider": "mock", "model": "m", "max_steps": 8,
            "format": 9
        }),
        ..existing
    };
    assert_eq!(newer.data["format"], json!(9));
    assert!(
        newer.parse().is_some(),
        "the header still parses, so the refusal is a decision and not a crash"
    );
}

/// TC-PROTO-106: contract section 4.3.3. An envelope change is a different
/// problem from a new type, and the two have different answers.
///
/// Section 4.3.2 lets the vocabulary grow because an unknown `type` is a line
/// a reader can skip while still understanding the rest of the journal. That
/// argument does not extend to the shape around it: a changed envelope is a
/// journal that cannot be read at all, or that reads into something meaning
/// something else. This case pins the distinction the two rules rest on.
#[test]
fn an_envelope_change_is_a_different_problem_from_a_new_type() {
    // A type from a later build: skipped, and everything around it still
    // works. This is section 4.3.2's whole promise.
    let unknown_type = SessionEvent {
        ty: "something/from-a-later-build".into(),
        seq: 5,
        time: 0,
        data: json!({ "whatever": true }),
        source_event_seqs: None,
    };
    assert!(unknown_type.parse().is_none(), "skipped, not fatal");
    assert_eq!(unknown_type.seq, 5, "and the envelope still reads");
    assert_eq!(unknown_type.data["whatever"], json!(true));

    // The envelope is what every line depends on, so it is the thing a
    // version has to protect: `seq` is how a journal is ordered and paged,
    // and a reader that misread it would page a history that never happened.
    let ordinary = SessionEvent {
        ty: "turn/start".into(),
        seq: 6,
        time: 7,
        data: json!({ "turn": 1 }),
        source_event_seqs: None,
    };
    let wire = serde_json::to_value(&ordinary).expect("serialize");
    let mut fields: Vec<&str> = wire
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        ["data", "seq", "time", "type"],
        "the envelope every line carries; `format` names which one this is"
    );
}
