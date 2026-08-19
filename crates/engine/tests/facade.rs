//! Conformance for the facade itself: the handshake, and the answer a call
//! gives before its slice lands.
//!
//! Test design: each case names the contract clause it fixes, and runs offline.

use tetanus_engine::HarnessEngine;
use tetanus_protocol::methods::{Engine, HelloParams, PeerInfo};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::PROTOCOL_VERSION;

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
    let engine = HarnessEngine::new();

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
    let engine = HarnessEngine::new();
    let error = engine
        .hello(hello("banana"))
        .await
        .expect_err("not a version");
    assert_eq!(error.kind(), Some(ErrorCode::UnsupportedProtocolVersion));
}

/// TC-ENG-3: every call this build does not serve answers `NotImplemented`
/// naming itself, so a surface can tell "not yet" from "went wrong".
#[tokio::test]
async fn unserved_calls_name_themselves() {
    let engine = HarnessEngine::new();
    let error = engine.catalog_tools().await.expect_err("not served yet");
    assert_eq!(error.kind(), Some(ErrorCode::NotImplemented));
    assert_eq!(
        error.data.expect("data")["method"],
        serde_json::json!("catalog.tools")
    );

    let error = engine.session_list().await.expect_err("not served yet");
    assert_eq!(
        error.data.expect("data")["method"],
        serde_json::json!("session.list")
    );
}
