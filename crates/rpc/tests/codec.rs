//! Conformance for the JSON-RPC codec's envelope and handshake.
//!
//! Test design: the codec is a translation, so every case asserts against
//! literal JSON on one side and a recorded engine call on the other. The
//! engine here is a scripted double, which is what lets a case pin *which*
//! trait method a method name reaches - something a real engine would hide
//! behind a plausible-looking answer. The calls this build does not dispatch
//! are `unreachable!` in the double, so wiring one without a case for it
//! fails loudly instead of passing quietly.
//!
//! Environmental needs: none. No case opens a file, a socket or a session.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tetanus_protocol::methods::{
    method, Ack, AgentPromptParams, AgentPromptResult, AgentStatusResult, ConfigDumpResult, Engine,
    EventSink, HelloParams, HelloResult, ModelCatalogResult, PeerInfo, SessionCreateParams,
    SessionEventsParams, SessionEventsResult, SessionListResult, SessionRef,
    SessionSubscribeParams, SessionSubscribeResult, SessionUnsubscribeParams, ToolCatalogResult,
};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types::SessionInfo;
use tetanus_protocol::PROTOCOL_VERSION;
use tetanus_rpc::Codec;

/// Records which trait method the codec reached, and with what.
#[derive(Default)]
struct Script {
    calls: Mutex<Vec<(&'static str, serde_json::Value)>>,
    /// When set, every call fails with this instead of answering.
    fail: Option<RpcError>,
}

impl Script {
    fn record<T: serde::Serialize>(&self, name: &'static str, params: T) -> Result<(), RpcError> {
        self.calls
            .lock()
            .expect("calls")
            .push((name, serde_json::to_value(params).expect("params")));
        match &self.fail {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn reached(&self) -> Vec<&'static str> {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .map(|(name, _)| *name)
            .collect()
    }
}

/// The answer to a call this build does not dispatch. Reaching one is a wiring
/// mistake, not a test failure to be asserted on.
fn undispatched<T>() -> T {
    unreachable!("this build dispatches only the handshake")
}

#[async_trait::async_trait]
impl Engine for Script {
    async fn hello(&self, params: HelloParams) -> Result<HelloResult, RpcError> {
        self.record(method::HELLO, params)?;
        Ok(HelloResult {
            protocol_version: PROTOCOL_VERSION.into(),
            server: PeerInfo {
                name: "tetanus".into(),
                version: "0".into(),
            },
            capabilities: vec!["session.subscribe".into()],
        })
    }

    async fn session_create(&self, _: SessionCreateParams) -> Result<SessionInfo, RpcError> {
        undispatched()
    }
    async fn session_events(
        &self,
        _: SessionEventsParams,
    ) -> Result<SessionEventsResult, RpcError> {
        undispatched()
    }
    async fn session_unsubscribe(&self, _: SessionUnsubscribeParams) -> Result<Ack, RpcError> {
        undispatched()
    }
    async fn agent_prompt(&self, _: AgentPromptParams) -> Result<AgentPromptResult, RpcError> {
        undispatched()
    }
    async fn agent_status(&self, _: SessionRef) -> Result<AgentStatusResult, RpcError> {
        undispatched()
    }
    async fn agent_interrupt(&self, _: SessionRef) -> Result<Ack, RpcError> {
        undispatched()
    }
    async fn session_list(&self) -> Result<SessionListResult, RpcError> {
        undispatched()
    }
    async fn session_subscribe(
        &self,
        _: SessionSubscribeParams,
        _: Arc<dyn EventSink>,
    ) -> Result<SessionSubscribeResult, RpcError> {
        undispatched()
    }
    async fn catalog_tools(&self) -> Result<ToolCatalogResult, RpcError> {
        undispatched()
    }
    async fn catalog_models(&self) -> Result<ModelCatalogResult, RpcError> {
        undispatched()
    }
    async fn config_dump(&self) -> Result<ConfigDumpResult, RpcError> {
        undispatched()
    }
}

fn hello(id: serde_json::Value) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method::HELLO,
        "params": {
            "protocol_version": PROTOCOL_VERSION,
            "client": { "name": "t", "version": "0" },
        },
    })
}

/// A codec whose handshake has already happened, and the engine behind it.
async fn greeted() -> (Codec, Arc<Script>) {
    let engine = Arc::new(Script::default());
    let codec = Codec::new(engine.clone());
    send(&codec, hello(json!(0))).await;
    engine.calls.lock().expect("calls").clear();
    (codec, engine)
}

async fn send(codec: &Codec, frame: serde_json::Value) -> serde_json::Value {
    let answer = codec
        .frame(&frame.to_string())
        .await
        .expect("a request is answered");
    serde_json::from_str(&answer).expect("an answer is JSON")
}

/// TC-RPC-1: a request is answered on the same id, with the engine's result
/// verbatim under `result`. The id is echoed exactly, so a client that numbers
/// its calls and one that names them both work.
#[tokio::test]
async fn a_request_is_answered_on_its_own_id() {
    let engine = Arc::new(Script::default());
    let codec = Codec::new(engine.clone());

    for id in [json!(7), json!("call-7")] {
        let answer = send(&codec, hello(id.clone())).await;
        assert_eq!(answer["jsonrpc"], "2.0", "every answer carries the version");
        assert_eq!(answer["id"], id, "the id is echoed exactly");
        assert_eq!(
            answer["result"]["protocol_version"], PROTOCOL_VERSION,
            "the engine's result is the answer's result"
        );
        assert!(answer.get("error").is_none(), "a result is not an error");
    }
    assert_eq!(engine.reached(), vec![method::HELLO, method::HELLO]);
}

/// TC-RPC-2: contract section 4.1. A frame the server cannot correlate is
/// still answered, with `id: null`, so a client that is waiting is released.
#[tokio::test]
async fn an_unreadable_frame_is_answered_with_a_null_id() {
    let codec = Codec::new(Arc::new(Script::default()));

    let unparseable = codec.frame("{ not json").await.expect("an answer");
    let unparseable: serde_json::Value = serde_json::from_str(&unparseable).expect("JSON");
    assert!(
        unparseable["id"].is_null(),
        "an unparseable frame has no id"
    );
    assert_eq!(unparseable["error"]["code"], ErrorCode::ParseError as i32);

    let batch = send(&codec, json!([hello(json!(1))])).await;
    assert!(batch["id"].is_null(), "a batch array has no one id");
    assert_eq!(batch["error"]["code"], ErrorCode::InvalidRequest as i32);

    let envelope = send(&codec, json!({ "jsonrpc": "2.0", "method": 7 })).await;
    assert!(envelope["id"].is_null(), "an unreadable envelope has no id");
    assert_eq!(envelope["error"]["code"], ErrorCode::InvalidRequest as i32);
}

/// TC-RPC-3: a frame that asked no question is not answered. A notification is
/// one-way, and a response answers a request this server made.
#[tokio::test]
async fn a_frame_that_asked_nothing_gets_no_answer() {
    let codec = Codec::new(Arc::new(Script::default()));

    let notification = json!({ "jsonrpc": "2.0", "method": "session.event" });
    let response = json!({ "jsonrpc": "2.0", "id": 1, "result": {} });
    for frame in [notification, response] {
        assert!(
            codec.frame(&frame.to_string()).await.is_none(),
            "nothing is written back to {frame}"
        );
    }
}

/// TC-RPC-4: contract section 4.4.1. `rpc.hello` is the first call on a
/// connection, and the codec refuses any other until the engine has accepted
/// one. The state is per connection, so a second codec starts ungreeted.
#[tokio::test]
async fn a_call_before_the_handshake_is_refused() {
    let engine = Arc::new(Script::default());
    let codec = Codec::new(engine.clone());
    let early = json!({ "jsonrpc": "2.0", "id": 1, "method": method::SESSION_LIST });

    let refused = send(&codec, early.clone()).await;
    assert_eq!(refused["id"], 1, "a refusal is still correlated");
    assert_eq!(refused["error"]["code"], ErrorCode::InvalidRequest as i32);
    assert!(
        engine.reached().is_empty(),
        "an ungreeted connection reaches no engine call"
    );

    // A refused handshake does not settle the version, so it does not open the
    // connection either.
    let rejecting = Arc::new(Script {
        fail: Some(RpcError::new(ErrorCode::InvalidParams, "version")),
        ..Script::default()
    });
    let rejected = Codec::new(rejecting);
    assert!(send(&rejected, hello(json!(0))).await["error"].is_object());
    assert_eq!(
        send(&rejected, early.clone()).await["error"]["code"],
        ErrorCode::InvalidRequest as i32,
        "a connection whose hello was refused is still ungreeted"
    );

    // The handshake is connection state, not process state.
    let (greeted, _) = greeted().await;
    assert!(
        send(&greeted, early).await["error"]["code"] != json!(ErrorCode::InvalidRequest as i32),
        "a greeted connection is past the gate"
    );
}

/// TC-RPC-5: contract section 4.5. An unknown method names itself in
/// `data.method`, and never reaches the engine.
#[tokio::test]
async fn an_unknown_method_names_itself() {
    let (codec, engine) = greeted().await;

    let answer = send(
        &codec,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "session.destroy" }),
    )
    .await;

    assert_eq!(answer["id"], 2);
    assert_eq!(answer["error"]["code"], ErrorCode::MethodNotFound as i32);
    assert_eq!(answer["error"]["data"]["method"], "session.destroy");
    assert!(engine.reached().is_empty(), "no engine call was reached");
}

/// TC-RPC-6: contract section 4.5. Params that do not fit the call are refused
/// with `InvalidParams` naming the field at fault, and never reach the engine.
#[tokio::test]
async fn bad_params_name_the_field_at_fault() {
    let engine = Arc::new(Script::default());
    let codec = Codec::new(engine.clone());

    let answer = send(
        &codec,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": method::HELLO,
            "params": { "protocol_version": PROTOCOL_VERSION },
        }),
    )
    .await;

    assert_eq!(answer["error"]["code"], ErrorCode::InvalidParams as i32);
    assert_eq!(
        answer["error"]["data"]["field"], "client",
        "the one field serde named is the one reported"
    );
    assert!(engine.reached().is_empty(), "no engine call was reached");
}
