//! Conformance for the facade itself: the handshake, and the answer a call
//! gives before its slice lands.
//!
//! Test design: each case names the contract clause it fixes, and runs offline.

use std::sync::Arc;

use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{
    capability, method, AgentPromptParams, AgentStatusPush, ApprovalSetParams, Engine, EventSink,
    HelloParams, PeerInfo, SessionCreateParams, SessionEventPush, SessionEventsParams,
    SessionForkParams, SessionRef, SessionSubscribeParams, SessionUnsubscribeParams,
};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::types::ApprovalPolicy;
use tetanus_protocol::PROTOCOL_VERSION;

/// A sink for a case that is asserting a call answers, not what it pushes.
struct Silent;

impl EventSink for Silent {
    fn session_event(&self, _: SessionEventPush) {}
    fn agent_status(&self, _: AgentStatusPush) {}
}

fn engine() -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });
    (engine, dir)
}

fn hello(version: &str) -> HelloParams {
    HelloParams {
        protocol_version: version.into(),
        client: PeerInfo {
            name: "test".into(),
            version: "0".into(),
        },
    }
}

/// TC-ENG-1: contract section 4.4.1. A matching major is accepted whatever the
/// minor; a different major is refused with the documented code and data.
#[tokio::test]
async fn hello_accepts_a_matching_major_only() {
    let (engine, _dir) = engine();

    let result = engine.hello(hello("1.7")).await.expect("a 1.x client");
    assert_eq!(result.protocol_version, PROTOCOL_VERSION);
    assert_eq!(result.server.name, "tetanus");

    let error = engine.hello(hello("2.0")).await.expect_err("a 2.x client");
    assert_eq!(error.kind(), Some(ErrorCode::UnsupportedProtocolVersion));
    let data = error.data.expect("data");
    assert_eq!(data["client"], serde_json::json!("2.0"));
    assert_eq!(data["server"], serde_json::json!(PROTOCOL_VERSION));
}

/// TC-ENG-2: a client that is not speaking `major.minor` at all is refused by
/// the same code, not accepted by accident.
#[tokio::test]
async fn hello_refuses_a_version_it_cannot_parse() {
    let (engine, _dir) = engine();
    let error = engine
        .hello(hello("banana"))
        .await
        .expect_err("not a version");
    assert_eq!(error.kind(), Some(ErrorCode::UnsupportedProtocolVersion));
}

/// TC-ENG-3: every call the contract's method table marks `Served` is served
/// by this build, so no caller meets `NotImplemented` where it was promised an
/// answer. This supersedes the earlier form of the case, which asserted the
/// "not yet" answer of the three read-only calls now that they are served. The
/// reserved rows are the other half of the table, and TC-ENG-4 has them.
#[tokio::test]
async fn every_call_in_the_table_is_served() {
    let (engine, _dir) = engine();
    engine.hello(hello(PROTOCOL_VERSION)).await.expect("hello");

    let info = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("session.create");
    let of = || SessionRef {
        session_id: info.session_id.clone(),
    };

    engine.session_list().await.expect("session.list");
    engine
        .session_events(SessionEventsParams {
            session_id: info.session_id.clone(),
            from_seq: 0,
            limit: None,
        })
        .await
        .expect("session.events");
    let subscribed = engine
        .session_subscribe(
            SessionSubscribeParams {
                session_id: info.session_id.clone(),
                from_seq: None,
            },
            Arc::new(Silent),
        )
        .await
        .expect("session.subscribe");
    engine
        .session_unsubscribe(SessionUnsubscribeParams {
            subscription_id: subscribed.subscription_id,
        })
        .await
        .expect("session.unsubscribe");

    engine
        .agent_prompt(AgentPromptParams {
            session_id: info.session_id.clone(),
            content: "hello".into(),
        })
        .await
        .expect("agent.prompt");
    engine.agent_status(of()).await.expect("agent.status");
    engine.agent_interrupt(of()).await.expect("agent.interrupt");

    engine.catalog_tools().await.expect("catalog.tools");
    engine.catalog_models().await.expect("catalog.models");
    engine.config_dump().await.expect("config.dump");
}

/// TC-ENG-4: contract section 4.2. A reserved call answers `NotImplemented`
/// and its capability is not advertised, which is the whole of what `Reserved`
/// promises a surface: the shape is frozen and may be compiled against, the
/// affordance is hidden until the capability appears, and a surface that calls
/// it anyway is told so in the one code section 4.5 gives that answer.
///
/// Input: every reserved call, on a session that exists, with params the
/// frozen shape accepts.
/// Expected: `NotImplemented` naming the method in `data`, and neither
/// capability in the handshake's. TC-SUB-5 asserts the complement - that every
/// capability which *is* advertised is served - so the two together leave no
/// room for a call to be half-promised.
#[tokio::test]
async fn a_reserved_call_answers_not_implemented_and_is_not_advertised() {
    let (engine, _dir) = engine();
    let capabilities = engine
        .hello(hello(PROTOCOL_VERSION))
        .await
        .expect("hello")
        .capabilities;
    let info = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("session.create");

    for reserved in [capability::SESSION_FORK, capability::APPROVAL_SET] {
        assert!(
            !capabilities.contains(&reserved.to_string()),
            "a reserved call must not be advertised: {capabilities:?}"
        );
    }

    let error = engine
        .session_fork(SessionForkParams {
            session_id: info.session_id.clone(),
            through_seq: None,
            child_session_id: None,
        })
        .await
        .expect_err("session.fork is reserved");
    assert_eq!(error.kind(), Some(ErrorCode::NotImplemented));
    assert_eq!(
        error.data.expect("data")["method"],
        serde_json::json!(method::SESSION_FORK)
    );

    let error = engine
        .approval_set(ApprovalSetParams {
            session_id: info.session_id,
            policy: ApprovalPolicy::Never,
        })
        .await
        .expect_err("approval.set is reserved");
    assert_eq!(error.kind(), Some(ErrorCode::NotImplemented));
    assert_eq!(
        error.data.expect("data")["method"],
        serde_json::json!(method::APPROVAL_SET)
    );
}
