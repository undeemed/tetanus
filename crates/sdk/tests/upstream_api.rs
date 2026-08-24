//! Test Design Specification: the request surface as data.
//!
//! Feature under test: `tetanus_sdk::gateway` - the descriptor catalog every
//! contract call is described in, the named-argument validation done against
//! it, and the dispatch behind it. This is upstream's `api/*`, which
//! `docs/parity.md` marks phase ③.
//!
//! Approach: the completeness cases read `tetanus_protocol::methods::method::ALL`
//! and iterate it, rather than naming calls one by one. A case that listed the
//! calls would have the same hole one level up as the hand-written routing arm
//! it is guarding: the call that gets forgotten is the one no case names. The
//! behaviour cases drive a real `HarnessEngine` on the offline mock adapter.
//!
//! Features NOT tested here: the answers themselves, which `crates/engine/tests`
//! owns, and the JSON-RPC envelope, which `crates/rpc/tests` owns. The gateway
//! is not a carrier and carries no envelope.
//!
//! Environmental needs: a writable temp directory. No case reaches a network or
//! an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use tempfile::TempDir;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{method, Engine};
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::PROTOCOL_VERSION;
use tetanus_sdk::gateway::{describe, DESCRIPTORS};
use tetanus_sdk::Gateway;

fn gateway(dir: &TempDir) -> Gateway {
    let engine: Arc<dyn Engine> = Arc::new(HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().join("sessions"),
        ..EngineConfig::default()
    }));
    Gateway::new(engine)
}

fn args(pairs: Value) -> Map<String, Value> {
    match pairs {
        Value::Object(map) => map,
        other => panic!("arguments are an object, not {other}"),
    }
}

/// TC-PORT-API-1: every call the contract enumerates is described.
///
/// Input: `method::ALL`, iterated.
/// Expected: each name has a descriptor. Asserted by iterating the contract's
/// own list rather than by naming the calls, because a case that named them
/// would have to be remembered the same way the routing arm it is guarding
/// would - and the one that gets forgotten is the one nobody named.
#[test]
fn every_contract_call_has_a_descriptor() {
    for name in method::ALL {
        assert!(
            describe(name).is_some(),
            "`{name}` is in the contract's method list and has no descriptor",
        );
    }
}

/// TC-PORT-API-2: nothing is described that the gateway does not route.
///
/// Input: every descriptor, invoked with its required arguments absent.
/// Expected: none answers `NotImplemented` with "described but this build
/// routes it nowhere" - the arm that catches a descriptor with no dispatch. A
/// descriptor for a call nobody serves would advertise a call that fails.
#[tokio::test]
async fn nothing_is_described_that_is_not_routed() {
    let dir = TempDir::new().expect("temp dir");
    let gateway = gateway(&dir);

    for descriptor in DESCRIPTORS {
        // An empty argument map reaches validation, not dispatch, for a call
        // with required arguments; a call with none reaches dispatch. Either
        // way the "described but routed nowhere" arm must not be the answer.
        let answered = gateway.invoke(descriptor.endpoint, Map::new()).await;
        if let Err(error) = answered {
            assert!(
                !error.message.contains("routes it nowhere"),
                "`{}` is described and dispatched nowhere",
                descriptor.endpoint,
            );
        }
    }
}

/// TC-PORT-API-3: `agent.steer` is described and routed although the
/// contract's own `method::ALL` omits it.
///
/// Input: the `agent.steer` descriptor, and the contract's list.
/// Expected: a descriptor exists and the call is routed to its reserved
/// answer, while `method::ALL` does not name it. The omission is a defect in
/// the shared contract crate, which this lane does not edit; it is proposed in
/// `docs/contract-updates/acp-gateway.md` and pinned here so that fixing it
/// there does not go unnoticed.
#[test]
fn the_reserved_steer_call_is_described_despite_its_absence_from_the_contract_list() {
    let descriptor = describe(method::AGENT_STEER).expect("described");
    assert!(descriptor.reserved);
    assert!(
        !method::ALL.contains(&method::AGENT_STEER),
        "`method::ALL` now names `agent.steer`; \
         retire the proposal in docs/contract-updates/acp-gateway.md",
    );
}

/// TC-PORT-API-4: a descriptor says what a caller must send.
///
/// Input: the descriptors for a call with required arguments, one with only
/// optional ones, and one with none.
/// Expected: each names exactly the contract's fields, with the right ones
/// required. This is the whole product: a caller reads the catalog instead of
/// the contract.
#[test]
fn a_descriptor_names_the_arguments_of_its_call() {
    let prompt = describe(method::AGENT_PROMPT).expect("described");
    assert_eq!(
        prompt
            .params
            .iter()
            .map(|param| (param.name, param.required))
            .collect::<Vec<_>>(),
        vec![("session_id", true), ("content", true)],
    );

    let create = describe(method::SESSION_CREATE).expect("described");
    assert!(
        create.params.iter().all(|param| !param.required),
        "every `session.create` field is optional",
    );
    assert!(create.accepts("provider"));
    assert!(!create.accepts("providerr"));

    assert!(describe(method::CONFIG_DUMP)
        .expect("described")
        .params
        .is_empty());
}

/// TC-PORT-API-5: a descriptor names the capability an optional call needs.
///
/// Input: the optional calls and a mandatory one.
/// Expected: each optional call names the `capability` string a server
/// advertises for it, and the mandatory one names none. A caller pairs the
/// catalog with the handshake to know what this build will actually do.
#[test]
fn a_descriptor_names_the_capability_an_optional_call_needs() {
    use tetanus_protocol::methods::capability;

    let expected = [
        (method::SESSION_FORK, Some(capability::SESSION_FORK)),
        (
            method::SESSION_SUBSCRIBE,
            Some(capability::SESSION_SUBSCRIBE),
        ),
        (method::AGENT_INTERRUPT, Some(capability::AGENT_INTERRUPT)),
        (method::AGENT_STEER, Some(capability::AGENT_STEER)),
        (method::APPROVAL_SET, Some(capability::APPROVAL_SET)),
        (method::AGENT_PROMPT, None),
        (method::SESSION_CREATE, None),
    ];
    for (endpoint, capability) in expected {
        assert_eq!(
            describe(endpoint).expect("described").capability,
            capability,
            "`{endpoint}`",
        );
    }
}

/// TC-PORT-API-6: an endpoint no contract call has is refused as unknown.
///
/// Input: `session.destroy`.
/// Expected: `MethodNotFound` naming it in `data.method` - the same answer the
/// codec gives an unknown method, because a caller that moved from one to the
/// other must not meet a new failure.
#[tokio::test]
async fn an_unknown_endpoint_is_method_not_found() {
    let dir = TempDir::new().expect("temp dir");
    let refused = gateway(&dir)
        .invoke("session.destroy", Map::new())
        .await
        .expect_err("no such endpoint");

    assert_eq!(refused.kind(), Some(ErrorCode::MethodNotFound));
    assert_eq!(refused.data, Some(json!({ "method": "session.destroy" })),);
}

/// TC-PORT-API-7: a missing required argument is refused, and named.
///
/// Input: `agent.prompt` with a session and no content.
/// Expected: `InvalidParams` with `data.field` naming `content`. Named rather
/// than described in prose because a surface renders the field, and a message
/// it has to parse is not a contract.
#[tokio::test]
async fn a_missing_required_argument_is_refused_and_named() {
    let dir = TempDir::new().expect("temp dir");
    let refused = gateway(&dir)
        .invoke(method::AGENT_PROMPT, args(json!({ "session_id": "s" })))
        .await
        .expect_err("content is required");

    assert_eq!(refused.kind(), Some(ErrorCode::InvalidParams));
    assert_eq!(refused.data, Some(json!({ "field": "content" })));
}

/// TC-PORT-API-8: an argument the call does not have is refused, and named.
///
/// Input: `session.create` with `provider_name`.
/// Expected: `InvalidParams` naming `provider_name`. This is the check worth
/// having: accepting an unrecognised argument silently turns a caller's typo
/// into a call that quietly did something else, and the caller has no way to
/// find out.
#[tokio::test]
async fn an_unrecognised_argument_is_refused_and_named() {
    let dir = TempDir::new().expect("temp dir");
    let refused = gateway(&dir)
        .invoke(
            method::SESSION_CREATE,
            args(json!({ "provider_name": "mock" })),
        )
        .await
        .expect_err("no such argument");

    assert_eq!(refused.kind(), Some(ErrorCode::InvalidParams));
    assert_eq!(refused.data, Some(json!({ "field": "provider_name" })));
}

/// TC-PORT-API-9: an argument of the wrong type is refused by the type that
/// owns it, and the refusal describes the mismatch rather than guessing at a
/// field name.
///
/// Input: `session.events` with a string `from_seq`.
/// Expected: `InvalidParams` whose message is the params type's own account of
/// the mismatch, and no `data.field`. Names are the descriptor's business and
/// values are the params type's, and `serde` names a key for a missing or
/// unknown field but only *describes* a wrong value. Inferring which argument
/// it meant would put a field name a surface renders as fact behind a guess, so
/// a value fault carries no `field` - which is exactly what contract section
/// 4.5's "when one field is at fault" leaves room for.
#[tokio::test]
async fn a_wrongly_typed_argument_is_refused_by_the_type_that_owns_it() {
    let dir = TempDir::new().expect("temp dir");
    let refused = gateway(&dir)
        .invoke(
            method::SESSION_EVENTS,
            args(json!({ "session_id": "s", "from_seq": "one" })),
        )
        .await
        .expect_err("from_seq is a number");

    assert_eq!(refused.kind(), Some(ErrorCode::InvalidParams));
    assert!(
        refused.message.contains("invalid type"),
        "describes the mismatch: {}",
        refused.message,
    );
    assert_eq!(refused.data, None, "no guessed field name");

    // The other half of the split: a *name* fault does carry the field, and
    // the gateway's own check is what makes that reliable rather than
    // dependent on how `serde` happened to phrase it.
    let named = gateway(&dir)
        .invoke(
            method::SESSION_EVENTS,
            args(json!({ "session_id": "s", "fromseq": 1 })),
        )
        .await
        .expect_err("no such argument");
    assert_eq!(named.data, Some(json!({ "field": "fromseq" })));
}

/// TC-PORT-API-10: a validated call reaches the engine and its answer comes
/// back as the contract's own JSON.
///
/// Input: a handshake, a session, a prompt, and a page of events - all through
/// the gateway.
/// Expected: each answer deserializes as the contract type it is, and the turn
/// really ran: the journal the gateway pages back holds the tool call.
#[tokio::test]
async fn a_validated_call_reaches_the_engine_and_answers_in_contract_shape() {
    let dir = TempDir::new().expect("temp dir");
    let gateway = gateway(&dir);

    let hello = gateway
        .invoke(
            method::HELLO,
            args(json!({
                "protocol_version": PROTOCOL_VERSION,
                "client": { "name": "gateway-case", "version": "0" },
            })),
        )
        .await
        .expect("handshake");
    assert_eq!(hello["protocol_version"], json!(PROTOCOL_VERSION));

    let created = gateway
        .invoke(method::SESSION_CREATE, Map::new())
        .await
        .expect("session");
    let session_id = created["session_id"].as_str().expect("an id").to_string();

    let prompted = gateway
        .invoke(
            method::AGENT_PROMPT,
            args(json!({ "session_id": session_id, "content": "hello gateway" })),
        )
        .await
        .expect("turn");
    assert_eq!(
        prompted["summary"]["content"],
        json!("You said: hello gateway")
    );

    let page = gateway
        .invoke(
            method::SESSION_EVENTS,
            args(json!({ "session_id": session_id })),
        )
        .await
        .expect("events");
    let events = page["events"].as_array().expect("a page");
    assert!(
        events
            .iter()
            .any(|event| event["type"] == json!("tool/call")),
        "the turn really ran",
    );
    assert_eq!(page["eof"], json!(true));
}

/// TC-PORT-API-11: the gateway dispatches unary calls, and says so about the
/// one that is not.
///
/// Input: `session.subscribe` through `invoke`.
/// Expected: `InvalidRequest` naming the method and pointing at the streaming
/// entry point. Opening a subscription into a sink nobody reads would be worse
/// than refusing: the engine would push into it for the life of the process
/// and nothing would ever say so.
#[tokio::test]
async fn a_streaming_call_is_refused_by_the_unary_entry_point() {
    let dir = TempDir::new().expect("temp dir");
    let gateway = gateway(&dir);
    gateway
        .invoke(
            method::HELLO,
            args(json!({
                "protocol_version": PROTOCOL_VERSION,
                "client": { "name": "c", "version": "0" },
            })),
        )
        .await
        .expect("handshake");
    let created = gateway
        .invoke(method::SESSION_CREATE, Map::new())
        .await
        .expect("session");
    let session_id = created["session_id"].as_str().expect("an id").to_string();

    let refused = gateway
        .invoke(
            method::SESSION_SUBSCRIBE,
            args(json!({ "session_id": session_id })),
        )
        .await
        .expect_err("a subscription needs a sink");

    assert_eq!(refused.kind(), Some(ErrorCode::InvalidRequest));
    assert!(
        refused.message.contains("invoke_streaming"),
        "says where to go: {}",
        refused.message,
    );
    assert!(
        describe(method::SESSION_SUBSCRIBE)
            .expect("described")
            .streaming
    );
}

/// TC-PORT-API-12: a caller with a sink may make the streaming call, and it
/// pushes.
///
/// Input: `session.subscribe` through `invoke_streaming`, then a turn.
/// Expected: the subscription is opened, its result is the contract's own
/// shape, and the turn's events reach the sink. Described *and* reachable: a
/// catalog entry that could not be called would be a lie.
#[tokio::test]
async fn the_streaming_entry_point_opens_a_subscription_that_delivers() {
    use std::sync::Mutex;
    use tetanus_protocol::methods::{AgentStatusPush, EventSink, SessionEventPush};

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);
    impl EventSink for Recorder {
        fn session_event(&self, push: SessionEventPush) {
            self.0.lock().expect("seen").push(push.event.ty);
        }
        fn agent_status(&self, _: AgentStatusPush) {}
    }

    let dir = TempDir::new().expect("temp dir");
    let gateway = gateway(&dir);
    gateway
        .invoke(
            method::HELLO,
            args(json!({
                "protocol_version": PROTOCOL_VERSION,
                "client": { "name": "c", "version": "0" },
            })),
        )
        .await
        .expect("handshake");
    let created = gateway
        .invoke(method::SESSION_CREATE, Map::new())
        .await
        .expect("session");
    let session_id = created["session_id"].as_str().expect("an id").to_string();

    let recorder = Arc::new(Recorder::default());
    let opened = gateway
        .invoke_streaming(
            method::SESSION_SUBSCRIBE,
            args(json!({ "session_id": session_id })),
            Arc::clone(&recorder) as Arc<dyn EventSink>,
        )
        .await
        .expect("subscribe");
    assert!(opened["subscription_id"].is_string());
    assert_eq!(opened["last_seq"], json!(0));

    gateway
        .invoke(
            method::AGENT_PROMPT,
            args(json!({ "session_id": session_id, "content": "hi" })),
        )
        .await
        .expect("turn");

    let seen = recorder.0.lock().expect("seen").clone();
    assert!(seen.contains(&"turn/end".to_string()), "{seen:?}");
}

/// TC-PORT-API-13: a reserved call is routed to its reserved answer rather
/// than reported unknown.
///
/// Input: `agent.steer` and `approval.set`, each with valid arguments.
/// Expected: `NotImplemented` naming the method - contract section 4.2's
/// answer - and never `MethodNotFound`. Serving one of these later moves this
/// case to whichever call is reserved then; it does not retire it.
#[tokio::test]
async fn a_reserved_call_is_routed_to_its_reserved_answer() {
    let dir = TempDir::new().expect("temp dir");
    let gateway = gateway(&dir);

    for (endpoint, arguments) in [
        (
            method::AGENT_STEER,
            json!({ "session_id": "s", "content": "x" }),
        ),
        (
            method::APPROVAL_SET,
            json!({ "session_id": "s", "policy": "ask" }),
        ),
    ] {
        let refused = gateway
            .invoke(endpoint, args(arguments))
            .await
            .expect_err("reserved calls are not served by this build");
        assert_eq!(
            refused.kind(),
            Some(ErrorCode::NotImplemented),
            "`{endpoint}` answered {refused:?}",
        );
        assert_eq!(refused.data, Some(json!({ "method": endpoint })));
    }
}

/// TC-PORT-API-14: the gateway holds no handshake state, and says so by
/// serving a call without one.
///
/// Input: `catalog.tools` with no prior `rpc.hello`.
/// Expected: it is served. The handshake is per *connection*, and a gateway is
/// not one; `tetanus_sdk::Client` is where connection state lives, and
/// TC-PORT-SDK-4 is where the rule is held. Two components each half-enforcing
/// it would be the worst of both.
#[tokio::test]
async fn the_gateway_is_not_a_connection_and_holds_no_handshake() {
    let dir = TempDir::new().expect("temp dir");
    let tools = gateway(&dir)
        .invoke(method::CATALOG_TOOLS, Map::new())
        .await
        .expect("served without a handshake");
    assert!(tools["tools"].is_array());
}
