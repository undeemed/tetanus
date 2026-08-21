//! Conformance for the JSON-RPC codec.
//!
//! Test design: the codec is a translation, so every case asserts against
//! literal JSON on one side and a recorded engine call on the other. The
//! engine here is a scripted double, which is what lets a case pin *which*
//! trait method a method name reaches - something a real engine would hide
//! behind a plausible-looking answer.
//!
//! Environmental needs: none. No case opens a file, a socket or a session.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tetanus_protocol::methods::{
    method, Ack, AgentPromptParams, AgentPromptResult, AgentStatusPush, AgentStatusResult,
    ConfigDumpResult, Engine, EventSink, HelloParams, HelloResult, ModelCatalogResult, PeerInfo,
    SessionCreateParams, SessionEventPush, SessionEventsParams, SessionEventsResult,
    SessionForkParams, SessionListResult, SessionRef, SessionSubscribeParams,
    SessionSubscribeResult, SessionUnsubscribeParams, ToolCatalogResult,
};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types::{AgentState, SessionInfo, StopReason, TurnSummary};
use tetanus_protocol::PROTOCOL_VERSION;
use tetanus_rpc::Codec;

/// Records which trait method the codec reached, and with what.
#[derive(Default)]
struct Script {
    calls: Mutex<Vec<(&'static str, serde_json::Value)>>,
    /// When set, every call fails with this instead of answering.
    fail: Option<RpcError>,
    /// Set when `session.subscribe` was given a sink.
    sink: Mutex<Option<Arc<dyn EventSink>>>,
    /// Subscription ids are handed out one at a time, as a real engine does,
    /// so a case can tell one subscription from another.
    subscriptions: AtomicU32,
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

    fn params(&self) -> serde_json::Value {
        self.calls
            .lock()
            .expect("calls")
            .last()
            .expect("a call")
            .1
            .clone()
    }
}

fn info() -> SessionInfo {
    SessionInfo {
        session_id: "s1".into(),
        path: "/tmp/s1.jsonl".into(),
        provider: "mock".into(),
        model: "mock-echo-1".into(),
        created_time: 0,
        last_seq: 0,
        title: None,
        state: AgentState::Idle,
    }
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
    async fn session_create(&self, params: SessionCreateParams) -> Result<SessionInfo, RpcError> {
        self.record(method::SESSION_CREATE, params)?;
        Ok(info())
    }
    async fn session_fork(&self, params: SessionForkParams) -> Result<SessionInfo, RpcError> {
        self.record(method::SESSION_FORK, params)?;
        Ok(info())
    }
    async fn session_list(&self) -> Result<SessionListResult, RpcError> {
        self.record(method::SESSION_LIST, json!({}))?;
        Ok(SessionListResult {
            sessions: vec![info()],
        })
    }
    async fn session_events(
        &self,
        params: SessionEventsParams,
    ) -> Result<SessionEventsResult, RpcError> {
        self.record(method::SESSION_EVENTS, params)?;
        Ok(SessionEventsResult {
            events: Vec::new(),
            next_seq: 0,
            eof: true,
        })
    }
    async fn session_subscribe(
        &self,
        params: SessionSubscribeParams,
        sink: Arc<dyn EventSink>,
    ) -> Result<SessionSubscribeResult, RpcError> {
        self.record(method::SESSION_SUBSCRIBE, params)?;
        *self.sink.lock().expect("sink") = Some(sink);
        Ok(SessionSubscribeResult {
            subscription_id: format!(
                "sub-{}",
                self.subscriptions.fetch_add(1, Ordering::Relaxed) + 1
            ),
            last_seq: 0,
        })
    }
    async fn session_unsubscribe(&self, params: SessionUnsubscribeParams) -> Result<Ack, RpcError> {
        self.record(method::SESSION_UNSUBSCRIBE, params)?;
        Ok(Ack { ok: true })
    }
    async fn agent_prompt(&self, params: AgentPromptParams) -> Result<AgentPromptResult, RpcError> {
        self.record(method::AGENT_PROMPT, params)?;
        Ok(AgentPromptResult {
            summary: TurnSummary {
                turn: 1,
                steps: 2,
                stop_reason: StopReason::Natural,
                stop_veto: None,
                content: "done".into(),
                duration_ms: None,
                usage: None,
            },
        })
    }
    async fn agent_status(&self, params: SessionRef) -> Result<AgentStatusResult, RpcError> {
        self.record(method::AGENT_STATUS, params)?;
        Ok(AgentStatusResult {
            status: AgentStatusPush {
                session_id: "s1".into(),
                state: AgentState::Idle,
                turn: None,
                step: None,
            },
        })
    }
    async fn agent_interrupt(&self, params: SessionRef) -> Result<Ack, RpcError> {
        self.record(method::AGENT_INTERRUPT, params)?;
        Ok(Ack { ok: false })
    }
    async fn catalog_tools(&self) -> Result<ToolCatalogResult, RpcError> {
        self.record(method::CATALOG_TOOLS, json!({}))?;
        Ok(ToolCatalogResult { tools: Vec::new() })
    }
    async fn catalog_models(&self) -> Result<ModelCatalogResult, RpcError> {
        self.record(method::CATALOG_MODELS, json!({}))?;
        Ok(ModelCatalogResult {
            providers: Vec::new(),
        })
    }
    async fn config_dump(&self) -> Result<ConfigDumpResult, RpcError> {
        self.record(method::CONFIG_DUMP, json!({}))?;
        Ok(ConfigDumpResult {
            entries: Vec::new(),
        })
    }
}

struct Discard;

impl EventSink for Discard {
    fn session_event(&self, _: SessionEventPush) {}
    fn agent_status(&self, _: AgentStatusPush) {}
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
        .frame(&frame.to_string(), Arc::new(Discard))
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

    let unparseable = codec
        .frame("{ not json", Arc::new(Discard))
        .await
        .expect("an answer");
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
            codec
                .frame(&frame.to_string(), Arc::new(Discard))
                .await
                .is_none(),
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
        send(&greeted, early).await.get("result").is_some(),
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

/// TC-RPC-12: contract section 4.2. A method the table names but this build
/// does not serve yet is routed, so it answers `NotImplemented` rather than
/// `MethodNotFound`.
///
/// The two codes are one character apart in a log and a whole decision apart
/// for a caller: section 4.5 exits 3 on the first, meaning "this build, not
/// this call", and 2 on the second, meaning the caller is wrong. A reserved
/// call that fell through to the unknown arm would tell every surface building
/// against the frozen shape that the shape does not exist.
///
/// The subject is whichever call is reserved now. `session.fork` was it until
/// the slice that served it, and a routing arm is added by hand, so the arm
/// that gets forgotten is the one no case names.
#[tokio::test]
async fn a_reserved_method_is_routed_rather_than_unknown() {
    let (codec, _engine) = greeted().await;

    let answer = send(
        &codec,
        json!({
            "jsonrpc": "2.0", "id": 4, "method": method::APPROVAL_SET,
            "params": { "session_id": "s1", "policy": "never" }
        }),
    )
    .await;

    assert_eq!(answer["id"], 4);
    assert_eq!(
        answer["error"]["code"],
        ErrorCode::NotImplemented as i32,
        "{answer}"
    );
    assert_eq!(answer["error"]["data"]["method"], method::APPROVAL_SET);
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

/// TC-RPC-7: an engine error crosses the boundary with its code, message and
/// data intact. The codec never remaps one code onto another.
#[tokio::test]
async fn an_engine_error_crosses_the_boundary_intact() {
    let engine = Arc::new(Script {
        fail: Some(
            RpcError::new(ErrorCode::UnsupportedProtocolVersion, "1.0 only")
                .with_data(json!({ "supported": ["1.0"] })),
        ),
        ..Script::default()
    });
    let codec = Codec::new(engine);

    let answer = send(&codec, hello(json!(4))).await;

    assert_eq!(answer["id"], 4, "a failed call is still correlated");
    assert_eq!(
        answer["error"]["code"],
        ErrorCode::UnsupportedProtocolVersion as i32
    );
    assert_eq!(answer["error"]["message"], "1.0 only");
    assert_eq!(answer["error"]["data"]["supported"][0], "1.0");
    assert!(answer.get("result").is_none(), "an error is not a result");
}

/// TC-RPC-8: contract section 4.2. Every method in the table reaches the trait
/// method of the same name, and no other. This is the case that stops a rename
/// or a mis-wired arm from turning into a plausible-looking wrong answer.
#[tokio::test]
async fn every_method_reaches_its_own_call() {
    let (codec, engine) = greeted().await;
    let calls = [
        (method::SESSION_CREATE, json!({})),
        (method::SESSION_LIST, json!({})),
        (
            method::SESSION_EVENTS,
            json!({ "session_id": "s1", "from_seq": 0 }),
        ),
        (method::SESSION_FORK, json!({ "session_id": "s1" })),
        (method::SESSION_SUBSCRIBE, json!({ "session_id": "s1" })),
        (
            method::SESSION_UNSUBSCRIBE,
            json!({ "subscription_id": "sub-1" }),
        ),
        (
            method::AGENT_PROMPT,
            json!({ "session_id": "s1", "content": "hi" }),
        ),
        (method::AGENT_STATUS, json!({ "session_id": "s1" })),
        (method::AGENT_INTERRUPT, json!({ "session_id": "s1" })),
        (method::CATALOG_TOOLS, json!({})),
        (method::CATALOG_MODELS, json!({})),
        (method::CONFIG_DUMP, json!({})),
    ];

    for (name, params) in &calls {
        let answer = send(
            &codec,
            json!({ "jsonrpc": "2.0", "id": 1, "method": name, "params": params }),
        )
        .await;
        assert!(
            answer.get("result").is_some(),
            "`{name}` answered an error: {answer}"
        );
    }

    let expected: Vec<&str> = calls.iter().map(|(name, _)| *name).collect();
    assert_eq!(engine.reached(), expected);
}

/// TC-RPC-9: the params a call receives are the params the frame carried, and
/// a subscription is given the carrier's sink, which the wire cannot supply.
#[tokio::test]
async fn params_cross_the_boundary_unchanged() {
    let (codec, engine) = greeted().await;

    send(
        &codec,
        json!({ "jsonrpc": "2.0", "id": 1, "method": method::SESSION_EVENTS,
                "params": { "session_id": "s1", "from_seq": 4, "limit": 2 } }),
    )
    .await;
    assert_eq!(
        engine.params(),
        json!({ "session_id": "s1", "from_seq": 4, "limit": 2 })
    );

    send(
        &codec,
        json!({ "jsonrpc": "2.0", "id": 2, "method": method::SESSION_SUBSCRIBE,
                "params": { "session_id": "s1" } }),
    )
    .await;
    assert!(
        engine.sink.lock().expect("sink").is_some(),
        "the carrier's sink reaches the engine, though no frame carried it"
    );
}

/// TC-RPC-10: contract section 4.2. A call with no params accepts an absent
/// `params` and `{}` alike, and a call whose params are all optional accepts
/// both too.
#[tokio::test]
async fn an_absent_params_and_an_empty_one_are_alike() {
    let (codec, _engine) = greeted().await;

    for params in [None, Some(json!({}))] {
        let mut frame = json!({ "jsonrpc": "2.0", "id": 1, "method": method::CATALOG_TOOLS });
        if let Some(params) = params.clone() {
            frame["params"] = params;
        }
        assert!(send(&codec, frame).await.get("result").is_some());

        let mut frame = json!({ "jsonrpc": "2.0", "id": 2, "method": method::SESSION_CREATE });
        if let Some(params) = params {
            frame["params"] = params;
        }
        let answer = send(&codec, frame).await;
        assert_eq!(answer["result"]["session_id"], json!("s1"));
    }
}

/// TC-RPC-11: a connection closes what it opened. `Codec::close` ends every
/// subscription the connection still holds, and only those: one already ended
/// by `session.unsubscribe` is not ended twice.
#[tokio::test]
async fn closing_a_connection_ends_its_subscriptions() {
    let (codec, engine) = greeted().await;
    for id in [1, 2] {
        send(
            &codec,
            json!({ "jsonrpc": "2.0", "id": id, "method": method::SESSION_SUBSCRIBE,
                    "params": { "session_id": "s1" } }),
        )
        .await;
    }
    send(
        &codec,
        json!({ "jsonrpc": "2.0", "id": 3, "method": method::SESSION_UNSUBSCRIBE,
                "params": { "subscription_id": "sub-1" } }),
    )
    .await;

    codec.close().await;

    assert_eq!(
        engine.reached(),
        vec![
            method::SESSION_SUBSCRIBE,
            method::SESSION_SUBSCRIBE,
            method::SESSION_UNSUBSCRIBE,
            method::SESSION_UNSUBSCRIBE,
        ],
        "the one still open is closed, and the one already closed is not"
    );
    assert_eq!(engine.params(), json!({ "subscription_id": "sub-2" }));

    codec.close().await;
    assert_eq!(
        engine.reached().len(),
        4,
        "a second close has nothing left to end"
    );
}
