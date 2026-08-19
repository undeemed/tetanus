//! Conformance for the session store: `session.create` and `session.list`.
//!
//! Test design: each case names the contract clause it fixes. Every case runs
//! offline against a temporary journal root, so none needs a key or a network.

use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{Engine, HelloParams, PeerInfo, SessionCreateParams};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::types::AgentState;
use tetanus_protocol::PROTOCOL_VERSION;

fn engine() -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    });
    (engine, dir)
}

/// TC-SESS-1: a fresh session opens an empty journal whose first line is the
/// header, so a cold reader needs no sidecar file.
#[tokio::test]
async fn a_new_session_opens_with_its_header() {
    let (engine, dir) = engine();
    let info = engine
        .session_create(SessionCreateParams {
            model: Some("some-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");

    assert_eq!(info.model, "some-model");
    assert_eq!(info.last_seq, 0, "the header is the only event");
    assert_eq!(info.state, AgentState::Idle);
    assert_eq!(
        info.path,
        dir.path()
            .join(format!("{}.jsonl", info.session_id))
            .display()
            .to_string()
    );
}

/// TC-SESS-2: reopening an id keeps the header the journal was created with.
/// The model a turn already ran under is a fact of the log, not of the call.
#[tokio::test]
async fn reopening_a_session_keeps_its_original_header() {
    let (engine, _dir) = engine();
    let first = engine
        .session_create(SessionCreateParams {
            session_id: Some("resume-me".into()),
            model: Some("first-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");

    let second = engine
        .session_create(SessionCreateParams {
            session_id: Some("resume-me".into()),
            model: Some("second-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("reopen");

    assert_eq!(second.model, "first-model");
    assert_eq!(second.session_id, first.session_id);
    assert_eq!(second.last_seq, 0, "reopening appends no second header");
}

/// TC-SESS-3: a cold journal left behind by a restart is listed from its own
/// header, with no live handle involved.
#[tokio::test]
async fn list_finds_cold_journals() {
    let dir = TempDir::new().expect("temp dir");
    let config = EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..EngineConfig::default()
    };

    let cold = HarnessEngine::new(config.clone());
    let created = cold
        .session_create(SessionCreateParams {
            model: Some("cold-model".into()),
            ..SessionCreateParams::default()
        })
        .await
        .expect("create");
    drop(cold);

    let restarted = HarnessEngine::new(config);
    let listed = restarted.session_list().await.expect("list").sessions;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_id, created.session_id);
    assert_eq!(listed[0].model, "cold-model");
    assert_eq!(listed[0].last_seq, 0);
}

/// TC-SESS-4: an id that would reach outside the journal root is
/// `InvalidParams`, never a filesystem error.
#[tokio::test]
async fn ids_that_name_a_path_are_rejected() {
    let (engine, _dir) = engine();

    for id in ["../escape", "with/slash", "..", ""] {
        let error = engine
            .session_create(SessionCreateParams {
                session_id: Some(id.into()),
                ..SessionCreateParams::default()
            })
            .await
            .expect_err("an id that names a path outside the root must be refused");
        assert_eq!(
            error.kind(),
            Some(ErrorCode::InvalidParams),
            "`{id}` must be InvalidParams"
        );
        assert_eq!(
            error.data.expect("data")["field"],
            serde_json::json!("session_id")
        );
    }
}

/// TC-SESS-5: a capability is a promise that the call behind it is served.
/// This build serves no optional call, so `rpc.hello` promises none.
#[tokio::test]
async fn no_capability_is_advertised_before_its_call_is_served() {
    let (engine, _dir) = engine();
    let result = engine
        .hello(HelloParams {
            protocol_version: PROTOCOL_VERSION.into(),
            client: PeerInfo {
                name: "test".into(),
                version: "0".into(),
            },
        })
        .await
        .expect("hello");
    assert!(
        result.capabilities.is_empty(),
        "advertised {:?} while the calls behind them answer NotImplemented",
        result.capabilities
    );
}
