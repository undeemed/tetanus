//! Test Design Specification: an ACP client driving a tetanus session.
//!
//! Feature under test: `tetanus_acp` - the Agent Client Protocol bridge, from
//! `initialize` to a completed prompt, over the workspace's own JSON-RPC
//! carrier. This is upstream's `acp/*`, which `docs/parity.md` marks phase ③.
//!
//! Approach: a client double drives the bridge the way a real ACP client
//! would, writing frames and reading frames and never calling a Rust method on
//! the bridge, against a real `HarnessEngine` on the offline mock adapter. The
//! double is deliberately dumb: it speaks JSON and knows nothing about this
//! workspace's types, so a case cannot pass by agreeing with itself about a
//! shape neither end serialises.
//!
//! The full-turn case runs over `tetanus_rpc::stdio::serve_handler` through a
//! real duplex stream, so what it asserts is bytes on a carrier. The other
//! cases drive `AcpBridge::frame` directly, because a case about one refusal
//! does not need a transport to state it.
//!
//! **Every wait in this file has a deadline.** A bridge is a thing that waits -
//! for a peer's answer, for a turn, for a frame - and the characteristic
//! failure of one is not a wrong value but a wait that never ends. An
//! unbounded wait in a suite turns that failure into a stalled lane with no
//! message, so [`within`] and [`until`] bound every one and fail loudly. Two
//! cases that need a turn held open use a gated provider rather than racing a
//! real one, because "wait until the other task happens to get there" is the
//! same unbounded wait wearing a disguise.
//!
//! Features NOT tested here: the turn itself (`crates/turn`), the engine's
//! answers (`crates/engine/tests`), and the carrier's own framing and
//! concurrency (`crates/rpc/tests`). None is restated.
//!
//! Environmental needs: a writable temp directory. No case reaches a network or
//! an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, a panic, or any wait exceeding its
//! deadline.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use tetanus_acp::wire::{method, PROTOCOL_VERSION};
use tetanus_acp::AcpBridge;
use tetanus_engine::agent::Providers;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::Engine;
use tetanus_protocol::rpc::ErrorCode;
use tetanus_protocol::types::ApprovalOutcome;
use tetanus_rpc::FrameSink;
use tetanus_turn::llm::{ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse};

/// How long any one wait in this suite may take before it is a hang.
///
/// Generous, because this box is shared and a loaded one is slow; but finite,
/// because the failure this bounds is a wait that would otherwise never end,
/// and twenty seconds of slow is indistinguishable from forever only if
/// nothing is watching.
const DEADLINE: Duration = Duration::from_secs(20);

/// Await something, or fail saying what did not finish.
async fn within<F: Future>(what: &str, future: F) -> F::Output {
    match tokio::time::timeout(DEADLINE, future).await {
        Ok(value) => value,
        Err(_) => {
            panic!("`{what}` did not finish within {DEADLINE:?}: this is a hang, not a slow box")
        }
    }
}

/// Wait for a condition, or fail saying which one never became true.
///
/// Polls rather than sleeps a fixed interval: the condition is a real state
/// signal - a flag the provider set, a frame the bridge wrote - so the loop
/// ends the moment the thing it is waiting for exists, and the deadline is
/// there only to turn "never" into a message.
async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "`{what}` never became true within {DEADLINE:?}",
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Collects everything the bridge wrote that was not an answer: notifications,
/// and requests the bridge made of the client.
#[derive(Default)]
struct Written(Mutex<Vec<Value>>);

impl Written {
    fn frames(&self) -> Vec<Value> {
        self.0.lock().expect("written").clone()
    }

    fn count(&self) -> usize {
        self.0.lock().expect("written").len()
    }

    /// The `session/update` payloads, in order.
    fn updates(&self) -> Vec<Value> {
        self.frames()
            .into_iter()
            .filter(|frame| frame["method"] == json!("session/update"))
            .map(|frame| frame["params"]["update"].clone())
            .collect()
    }
}

impl FrameSink for Written {
    fn send_frame(&self, frame: String) {
        self.0
            .lock()
            .expect("written")
            .push(serde_json::from_str(&frame).expect("the bridge writes JSON"));
    }
}

fn engine(dir: &TempDir) -> Arc<dyn Engine> {
    Arc::new(HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().join("sessions"),
        ..EngineConfig::default()
    }))
}

/// A provider that stops inside the model call until a case lets it go.
///
/// This is how a case observes a session whose turn is genuinely in flight.
/// The alternative - start a real turn and race it - is a wait with no signal
/// behind it, which is exactly the unbounded wait this suite refuses to
/// contain.
struct GateAdapter {
    entered: Arc<AtomicBool>,
    gate: Arc<tokio::sync::Semaphore>,
}

#[async_trait::async_trait]
impl LlmAdapter for GateAdapter {
    fn provider(&self) -> &str {
        "gate"
    }
    fn models(&self) -> Vec<String> {
        vec!["gate-1".to_string()]
    }
    async fn stream(
        &self,
        _request: &ModelRequest,
        _sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError> {
        self.entered.store(true, Ordering::Release);
        self.gate.acquire().await.expect("gate").forget();
        Ok(ModelResponse {
            content: "held, then answered".into(),
            finish_reason: "stop".into(),
            ..ModelResponse::default()
        })
    }
}

struct OneProvider(Arc<dyn LlmAdapter>);

impl Providers for OneProvider {
    fn all(&self) -> Vec<Arc<dyn LlmAdapter>> {
        vec![Arc::clone(&self.0)]
    }
}

/// A bridge, its out-of-band frames, and a request counter.
struct Client {
    bridge: AcpBridge,
    out: Arc<dyn FrameSink>,
    written: Arc<Written>,
    next: Mutex<i64>,
}

/// A held-open turn: the flag the provider sets on arrival, and the permit
/// that lets it finish.
struct Gate {
    entered: Arc<AtomicBool>,
    release: Arc<tokio::sync::Semaphore>,
}

impl Gate {
    /// Wait until a turn has actually reached the model call.
    async fn wait(&self, what: &str) {
        until(what, || self.entered.load(Ordering::Acquire)).await;
    }

    fn open(&self) {
        self.release.add_permits(1);
    }
}

impl Client {
    fn with(engine: Arc<dyn Engine>) -> Self {
        let written = Arc::new(Written::default());
        Self {
            bridge: AcpBridge::new(engine),
            out: Arc::clone(&written) as Arc<dyn FrameSink>,
            written,
            next: Mutex::new(1),
        }
    }

    fn new(dir: &TempDir) -> Self {
        Self::with(engine(dir))
    }

    /// A client whose turns stop inside the provider until the gate is opened.
    fn gated(dir: &TempDir) -> (Self, Gate) {
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let adapter: Arc<dyn LlmAdapter> = Arc::new(GateAdapter {
            entered: Arc::clone(&entered),
            gate: Arc::clone(&release),
        });
        let engine: Arc<dyn Engine> = Arc::new(HarnessEngine::new(EngineConfig {
            sessions_root: dir.path().join("sessions"),
            default_provider: "gate".into(),
            default_model: "gate-1".into(),
            providers: Arc::new(OneProvider(adapter)),
            ..EngineConfig::default()
        }));
        (Self::with(engine), Gate { entered, release })
    }

    /// Send one request and read its answer, as a client would.
    async fn call(&self, method: &str, params: Value) -> Value {
        let id = {
            let mut next = self.next.lock().expect("next");
            *next += 1;
            *next
        };
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let answered = within(
            &format!("the answer to `{method}`"),
            self.bridge.frame(&frame.to_string(), &self.out),
        )
        .await
        .expect("a request is answered");
        let answered: Value = serde_json::from_str(&answered).expect("JSON");
        assert_eq!(answered["id"], json!(id), "the answer echoes the id");
        answered
    }

    async fn notify(&self, method: &str, params: Value) {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let answered = within(
            &format!("the handling of the `{method}` notification"),
            self.bridge.frame(&frame.to_string(), &self.out),
        )
        .await;
        assert_eq!(answered, None, "a notification is not answered");
    }

    /// Answer a request the bridge made of us.
    async fn reply(&self, id: &Value, payload: Value) {
        let mut frame = json!({ "jsonrpc": "2.0", "id": id.clone() });
        for (key, value) in payload.as_object().expect("an object") {
            frame[key] = value.clone();
        }
        within(
            "the handling of a client response",
            self.bridge.frame(&frame.to_string(), &self.out),
        )
        .await;
    }

    /// The next frame the bridge wrote out of band after `seen`, waiting for
    /// one to appear.
    ///
    /// Indexed rather than "the last frame": a case that asks twice would
    /// otherwise be handed the *previous* question again, answer that dead id,
    /// and leave the new waiter with nothing coming. That is exactly the hang
    /// this suite is built to make impossible.
    async fn next_written(&self, seen: usize) -> Value {
        until("a frame from the bridge", || self.written.count() > seen).await;
        self.written.frames()[seen].clone()
    }

    async fn initialize(&self) -> Value {
        self.call(
            method::INITIALIZE,
            json!({ "protocolVersion": PROTOCOL_VERSION }),
        )
        .await
    }

    async fn new_session(&self) -> String {
        let answered = self
            .call(
                method::SESSION_NEW,
                json!({ "cwd": "/tmp", "mcpServers": [] }),
            )
            .await;
        answered["result"]["sessionId"]
            .as_str()
            .expect("a session id")
            .to_string()
    }
}

/// The error object of an answer that is one, with a readable panic when it is
/// not.
fn error_of(answered: &Value) -> &Value {
    assert!(
        answered.get("error").is_some(),
        "expected a refusal, got {answered}",
    );
    &answered["error"]
}

fn prompt_of(session_id: &str, text: &str) -> Value {
    json!({
        "sessionId": session_id,
        "prompt": [{ "type": "text", "text": text }],
    })
}

/// TC-PORT-ACP-1: an ACP client completes a whole turn over the carrier,
/// including a tool call and its result.
///
/// Input: a client speaking ACP down a real duplex stream into
/// `tetanus_rpc::stdio::serve_handler` - `initialize`, `session/new`,
/// `session/prompt` - against a real engine on the offline mock adapter.
/// Expected: the prompt answers `end_turn`; the client saw, in order, the
/// assistant's first message, a `tool_call` for `echo`, a `tool_call_update`
/// completing it and carrying its output, and the assistant's answer. Every
/// frame crossed the stream as bytes, so this is the protocol working and not
/// two Rust types agreeing. Every read is bounded: a carrier that stops
/// answering fails this case rather than stalling it.
#[tokio::test]
async fn an_acp_client_completes_a_whole_turn_over_the_carrier() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let dir = TempDir::new().expect("temp dir");
    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (from_server, mut to_server) = tokio::io::split(client_side);
    let (server_in, server_out) = tokio::io::split(server_side);

    let served = tokio::spawn(tetanus_acp::serve(engine(&dir), server_in, server_out));
    let mut reader = tokio::io::BufReader::new(from_server).lines();

    async fn send(to_server: &mut (impl AsyncWriteExt + Unpin), frame: Value) {
        within(
            "writing a frame",
            to_server.write_all(format!("{frame}\n").as_bytes()),
        )
        .await
        .expect("write a frame");
    }

    send(
        &mut to_server,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": PROTOCOL_VERSION },
        }),
    )
    .await;
    let hello = within("the initialize answer", reader.next_line())
        .await
        .expect("a readable stream")
        .expect("the carrier answered");
    let hello: Value = serde_json::from_str(&hello).expect("JSON");
    assert_eq!(hello["result"]["protocolVersion"], json!(PROTOCOL_VERSION));

    send(
        &mut to_server,
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "session/new",
            "params": { "cwd": "/tmp", "mcpServers": [] },
        }),
    )
    .await;
    let created = within("the session/new answer", reader.next_line())
        .await
        .expect("a readable stream")
        .expect("the carrier answered");
    let created: Value = serde_json::from_str(&created).expect("JSON");
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("an id")
        .to_string();

    send(
        &mut to_server,
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "hello acp" }],
            },
        }),
    )
    .await;

    // Read until the prompt's answer arrives; everything before it is a
    // notification, and its arriving first is the carrier's ordering promise.
    // The read is bounded, so a turn that never closes is a failure here and
    // not a wedged lane.
    let mut updates: Vec<Value> = Vec::new();
    let answer = loop {
        let frame = within("the next frame of the turn", reader.next_line())
            .await
            .expect("a readable stream")
            .expect("the carrier is still open");
        let frame: Value = serde_json::from_str(&frame).expect("JSON");
        if frame["id"] == json!(3) {
            break frame;
        }
        assert_eq!(frame["method"], json!("session/update"));
        assert_eq!(frame["params"]["sessionId"], json!(session_id));
        updates.push(frame["params"]["update"].clone());
    };

    assert_eq!(answer["result"]["stopReason"], json!("end_turn"));

    let kinds: Vec<&str> = updates
        .iter()
        .map(|update| update["sessionUpdate"].as_str().expect("a kind"))
        .collect();
    assert_eq!(
        kinds,
        vec![
            "agent_message_chunk",
            "tool_call",
            "tool_call_update",
            "agent_message_chunk",
        ],
        "the documented mock turn, in order: {updates:#?}",
    );

    assert_eq!(updates[1]["title"], json!("echo"));
    assert_eq!(updates[1]["status"], json!("pending"));
    assert_eq!(updates[1]["rawInput"]["text"], json!("hello acp"));
    assert_eq!(
        updates[2]["toolCallId"], updates[1]["toolCallId"],
        "the result names the call it answers",
    );
    assert_eq!(updates[2]["status"], json!("completed"));
    assert_eq!(
        updates[2]["content"][0]["content"]["text"],
        json!("hello acp"),
        "the tool's output reached the client",
    );
    assert_eq!(
        updates[3]["content"]["text"],
        json!("You said: hello acp"),
        "the turn's answer",
    );

    // Hanging up ends the connection; the carrier returns once every frame in
    // flight is answered. Bounded, because a carrier that does not return is
    // the failure this asserts against.
    drop(to_server);
    drop(reader);
    within("the carrier shutting down", served)
        .await
        .expect("the task joins")
        .expect("served");
}

/// TC-PORT-ACP-2: `initialize` states one version, no authentication method,
/// and no prompt capability.
///
/// Input: an `initialize` naming a version this agent does not speak.
/// Expected: the agent answers its own version anyway - a single-version agent
/// has nothing to negotiate - names itself, offers no `authMethods`, and
/// advertises all three prompt capabilities false. Advertising one it cannot
/// honour would move the failure from here, where a client can adapt, to the
/// middle of a prompt.
#[tokio::test]
async fn initialize_states_one_version_and_no_capabilities() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(&dir);

    let answered = client
        .call(method::INITIALIZE, json!({ "protocolVersion": 99 }))
        .await;
    let result = &answered["result"];

    assert_eq!(result["protocolVersion"], json!(PROTOCOL_VERSION));
    assert_eq!(result["agentInfo"]["name"], json!("tetanus-acp"));
    assert_eq!(result["authMethods"], json!([]));
    assert_eq!(
        result["agentCapabilities"]["promptCapabilities"],
        json!({ "image": false, "audio": false, "embeddedContext": false }),
    );
}

/// TC-PORT-ACP-3: every call but `initialize` is refused before it.
///
/// Input: `session/new` on a connection that has not initialized.
/// Expected: `InvalidRequest` naming `initialize`; and the same call served
/// afterwards. ACP makes `initialize` the first call, exactly as contract
/// section 4.4.1 makes `rpc.hello` the first call on a tetanus connection, and
/// the bridge holds its own protocol's rule rather than borrowing the other's.
#[tokio::test]
async fn a_call_before_initialize_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(&dir);

    let refused = client
        .call(method::SESSION_NEW, json!({ "cwd": "/tmp" }))
        .await;
    let error = error_of(&refused);
    assert_eq!(error["code"], json!(ErrorCode::InvalidRequest.code()));
    assert!(
        error["message"]
            .as_str()
            .expect("a message")
            .contains("initialize"),
        "{error}",
    );

    client.initialize().await;
    assert!(!client.new_session().await.is_empty(), "served after it");
}

/// TC-PORT-ACP-4: `authenticate` is a no-op.
///
/// Input: `authenticate` after `initialize`.
/// Expected: an empty result. No authentication method was advertised, so
/// there is nothing to check and refusing would fail a client that is being
/// polite.
#[tokio::test]
async fn authenticate_is_a_no_op_because_nothing_was_advertised() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(&dir);
    client.initialize().await;

    let answered = client.call(method::AUTHENTICATE, json!({})).await;
    assert_eq!(answered["result"], json!({}));
}

/// TC-PORT-ACP-5: a session must name an absolute working directory, and may
/// mount no MCP servers.
///
/// Input: `session/new` with a relative `cwd`, then with a non-empty
/// `mcpServers`, then correctly.
/// Expected: the first two are refused, each naming the field; the third
/// succeeds. A relative `cwd` resolves against this process's directory rather
/// than the client's, and the mistake is invisible until a tool reads the wrong
/// file.
#[tokio::test]
async fn a_session_needs_an_absolute_cwd_and_no_mcp_servers() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(&dir);
    client.initialize().await;

    let relative = client
        .call(method::SESSION_NEW, json!({ "cwd": "workspace" }))
        .await;
    assert_eq!(error_of(&relative)["data"], json!({ "field": "cwd" }));

    let mounted = client
        .call(
            method::SESSION_NEW,
            json!({ "cwd": "/tmp", "mcpServers": [{ "name": "x" }] }),
        )
        .await;
    assert_eq!(error_of(&mounted)["data"], json!({ "field": "mcpServers" }));

    assert!(!client.new_session().await.is_empty());
}

/// TC-PORT-ACP-6: a prompt naming a session this connection did not open is
/// refused.
///
/// Input: `session/prompt` for an id nobody created.
/// Expected: `InvalidParams` naming the session. Loading and resuming are not
/// part of this bridge, so an id it did not mint is one it cannot vouch for -
/// and serving it would let a client reach another connection's session by
/// guessing.
#[tokio::test]
async fn a_prompt_for_an_unknown_session_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(&dir);
    client.initialize().await;

    let refused = client
        .call(
            method::SESSION_PROMPT,
            prompt_of("somebody-elses-session", "hi"),
        )
        .await;

    let error = error_of(&refused);
    assert_eq!(error["code"], json!(ErrorCode::InvalidParams.code()));
    assert!(
        error["message"]
            .as_str()
            .expect("a message")
            .contains("unknown session"),
        "{error}",
    );
}

/// TC-PORT-ACP-7: prompt admission keeps order, flattens a resource link, and
/// refuses everything the agent did not advertise.
///
/// Input: each malformed prompt in turn, then a well-formed mixed one.
/// Expected: an empty prompt, a whitespace-only prompt, an image, audio, an
/// embedded resource and an unknown block are each refused, the block kinds
/// naming themselves in `data.kind`; and the mixed prompt runs a turn whose
/// user message is the blocks in order with the link bracketed. Admission is
/// all-or-nothing: a half-admitted prompt would leave a user message on the
/// journal describing a turn that never ran.
#[tokio::test]
async fn prompt_admission_preserves_order_and_refuses_what_was_not_advertised() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(&dir);
    client.initialize().await;
    let session_id = client.new_session().await;

    let refusals = [
        (json!([]), None),
        (json!([{ "type": "text", "text": "   " }]), None),
        (
            json!([{ "type": "image", "data": "AA", "mimeType": "image/png" }]),
            Some("image"),
        ),
        (
            json!([{ "type": "audio", "data": "AA", "mimeType": "audio/wav" }]),
            Some("audio"),
        ),
        (
            json!([{ "type": "resource", "resource": { "text": "x" } }]),
            Some("resource"),
        ),
        (json!([{ "type": "hologram" }]), Some("hologram")),
    ];
    for (prompt, kind) in refusals {
        let refused = client
            .call(
                method::SESSION_PROMPT,
                json!({ "sessionId": session_id, "prompt": prompt }),
            )
            .await;
        let error = error_of(&refused);
        assert_eq!(
            error["code"],
            json!(ErrorCode::InvalidParams.code()),
            "{error}",
        );
        match kind {
            Some(kind) => assert_eq!(error["data"], json!({ "kind": kind })),
            None => assert_eq!(error["data"], Value::Null, "no one block is at fault"),
        }
    }

    let answered = client
        .call(
            method::SESSION_PROMPT,
            json!({
                "sessionId": session_id,
                "prompt": [
                    { "type": "text", "text": "read this" },
                    { "type": "resource_link", "name": "notes", "uri": "file:///notes.md" },
                    { "type": "text", "text": "and summarise" },
                ],
            }),
        )
        .await;
    assert_eq!(answered["result"]["stopReason"], json!("end_turn"));

    // The mock adapter echoes the user message back, so the turn's own answer
    // is the proof of what was admitted.
    let echoed = client
        .written
        .updates()
        .into_iter()
        .filter_map(|update| update["content"]["text"].as_str().map(str::to_string))
        .find(|text| text.starts_with("You said:"))
        .expect("the turn answered");
    assert_eq!(
        echoed,
        "You said: read this\n[resource_link name=notes uri=file:///notes.md]\nand summarise",
        "order kept, link flattened, nothing fetched",
    );
}

/// TC-PORT-ACP-8: one prompt in flight per session.
///
/// Input: a turn held open inside the provider, then a second `session/prompt`
/// for the same session.
/// Expected: the second is refused as already in flight, and the first still
/// answers `end_turn` once released. The turn is held by a gate rather than
/// raced, so "the first prompt has the slot" is a fact the case established
/// and not a timing it hoped for. Without the slot the two would race the
/// engine's own one-turn-at-a-time rule, and the loser would get `SessionBusy`,
/// a tetanus code an ACP client has no way to interpret.
#[tokio::test]
async fn one_prompt_at_a_time_per_session() {
    let dir = TempDir::new().expect("temp dir");
    let (client, gate) = Client::gated(&dir);
    let client = Arc::new(client);
    client.initialize().await;
    let session_id = client.new_session().await;

    let first = {
        let client = Arc::clone(&client);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            client
                .call(method::SESSION_PROMPT, prompt_of(&session_id, "one"))
                .await
        })
    };
    gate.wait("the first prompt reaching the model").await;

    let refused = client
        .call(method::SESSION_PROMPT, prompt_of(&session_id, "two"))
        .await;
    assert!(
        error_of(&refused)["message"]
            .as_str()
            .expect("a message")
            .contains("already in flight"),
        "{refused}",
    );

    gate.open();
    let answered = within("the first prompt", first)
        .await
        .expect("the first prompt joins");
    assert_eq!(answered["result"]["stopReason"], json!("end_turn"));
}

/// TC-PORT-ACP-9: `session/cancel` is a notification, and an unknown session is
/// a no-op.
///
/// Input: a cancel for a session that exists and one for a session that does
/// not, neither with a prompt in flight.
/// Expected: neither is answered, and neither fails. ACP's cancel is one-way,
/// and a client racing its own teardown must not be answered with an error it
/// has nowhere to put.
#[tokio::test]
async fn cancel_is_one_way_and_an_unknown_session_is_a_no_op() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(&dir);
    client.initialize().await;
    let session_id = client.new_session().await;

    client
        .notify(method::SESSION_CANCEL, json!({ "sessionId": session_id }))
        .await;
    client
        .notify(method::SESSION_CANCEL, json!({ "sessionId": "nobody" }))
        .await;

    assert!(
        client.written.frames().is_empty(),
        "a notification produces no frame",
    );
}

/// TC-PORT-ACP-10: a cancelled prompt settles `cancelled`, whatever the turn
/// itself reported.
///
/// Input: a turn held open inside the provider, cancelled while it is held,
/// then released so it ends of its own accord.
/// Expected: the prompt answers `cancelled`. Explicit cancellation outranks the
/// turn's own reason: the client asked for the turn to stop, and whether it
/// happened to reach a natural end first is not the answer to that ask. The
/// gate is what makes "while it is held" true rather than hoped for.
#[tokio::test]
async fn an_explicitly_cancelled_prompt_settles_cancelled() {
    let dir = TempDir::new().expect("temp dir");
    let (client, gate) = Client::gated(&dir);
    let client = Arc::new(client);
    client.initialize().await;
    let session_id = client.new_session().await;

    let running = {
        let client = Arc::clone(&client);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            client
                .call(method::SESSION_PROMPT, prompt_of(&session_id, "work"))
                .await
        })
    };
    gate.wait("the prompt reaching the model").await;

    client
        .notify(method::SESSION_CANCEL, json!({ "sessionId": session_id }))
        .await;
    gate.open();

    let answered = within("the cancelled prompt", running)
        .await
        .expect("the prompt joins");
    assert_eq!(answered["result"]["stopReason"], json!("cancelled"));
}

/// TC-PORT-ACP-11: the stop-reason mapping is total, and says what it means.
///
/// Input: each of this workspace's stop reasons, including the growth
/// fallback.
/// Expected: natural is `end_turn`; a step cap is `max_turn_requests`, not
/// `max_tokens` - what ran out was the driver's budget of model requests, and a
/// client told `max_tokens` would retry with a shorter prompt and be wrong
/// about why; an interrupt is `cancelled`; and a harness rejection is
/// `end_turn`, because ACP's `refusal` means the *model* declined and reporting
/// a harness decision as one would put words in the model's mouth.
#[test]
fn the_stop_reason_mapping_is_total() {
    use tetanus_acp::StopReason as Acp;
    use tetanus_protocol::types::StopReason as Harness;

    assert_eq!(Acp::of(&Harness::Natural), Acp::EndTurn);
    assert_eq!(Acp::of(&Harness::MaxSteps), Acp::MaxTurnRequests);
    assert_eq!(Acp::of(&Harness::Cancelled), Acp::Cancelled);
    assert_eq!(Acp::of(&Harness::PreStepRejected), Acp::EndTurn);
    assert_eq!(
        Acp::of(&Harness::Other("something-new".into())),
        Acp::EndTurn,
        "a word this build does not know is quiescence, not a refusal",
    );
}

/// TC-PORT-ACP-12: uncommitted stream chunks never reach the client.
///
/// Input: one turn, and every update it produced.
/// Expected: two committed messages, not the five deltas the adapter streamed.
/// A chunk can be superseded by a retry and its text arrives again, whole, on
/// the message that closes the step, so forwarding both would show a client the
/// same sentence twice and could leak text from an attempt that was thrown
/// away.
#[tokio::test]
async fn only_committed_messages_reach_the_client() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(&dir);
    client.initialize().await;
    let session_id = client.new_session().await;

    client
        .call(method::SESSION_PROMPT, prompt_of(&session_id, "hi"))
        .await;

    let texts: Vec<String> = client
        .written
        .updates()
        .into_iter()
        .filter(|update| update["sessionUpdate"] == json!("agent_message_chunk"))
        .map(|update| {
            update["content"]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();

    assert_eq!(
        texts,
        vec!["Let me echo that back.", "You said: hi"],
        "committed messages only",
    );
}

/// TC-PORT-ACP-13: a permission question offers two one-shot choices, and every
/// way of not choosing `allow-once` denies.
///
/// Input: each client answer in turn - allow, reject, withdraw, an option the
/// agent never offered, and a JSON-RPC error.
/// Expected: only `allow-once` grants. An option the agent did not offer is
/// `Unavailable` rather than a guess in the client's favour: a client answering
/// `always-allow` to a question that offered two one-shot choices has either
/// misunderstood or is a different implementation, and neither is a grant this
/// bridge can honour.
#[tokio::test]
async fn a_permission_question_is_one_shot_and_fails_closed() {
    let dir = TempDir::new().expect("temp dir");

    let answers: Vec<(Value, ApprovalOutcome)> = vec![
        (
            json!({ "result": { "outcome": { "outcome": "selected", "optionId": "allow-once" } } }),
            ApprovalOutcome::AllowedOnce,
        ),
        (
            json!({ "result": { "outcome": { "outcome": "selected", "optionId": "reject-once" } } }),
            ApprovalOutcome::Rejected,
        ),
        (
            json!({ "result": { "outcome": { "outcome": "cancelled" } } }),
            ApprovalOutcome::Cancelled,
        ),
        (
            json!({ "result": { "outcome": { "outcome": "selected", "optionId": "always-allow" } } }),
            ApprovalOutcome::Unavailable,
        ),
        (
            json!({ "error": { "code": -32603, "message": "no" } }),
            ApprovalOutcome::Unavailable,
        ),
    ];

    for (answer, expected) in answers {
        let client = Arc::new(Client::new(&dir));
        let asking = {
            let client = Arc::clone(&client);
            tokio::spawn(async move {
                client
                    .bridge
                    .request_permission("s", "call_1", &client.out)
                    .await
            })
        };

        let asked = client.next_written(0).await;
        assert_eq!(asked["method"], json!("session/request_permission"));
        assert_eq!(asked["params"]["toolCall"]["toolCallId"], json!("call_1"));
        let offered: Vec<&str> = asked["params"]["options"]
            .as_array()
            .expect("options")
            .iter()
            .map(|option| option["optionId"].as_str().expect("an id"))
            .collect();
        assert_eq!(offered, vec!["allow-once", "reject-once"], "one-shot only");

        client.reply(&asked["id"], answer).await;
        assert_eq!(
            within("the permission answer", asking)
                .await
                .expect("joins"),
            expected,
        );
    }
}

/// TC-PORT-ACP-14: a request this side made carries an id no client could
/// collide with.
///
/// Input: two permission questions in a row on one connection.
/// Expected: both ids are strings with the bridge's own prefix, and they
/// differ. A numeric id could collide with the client's own numbering, and a
/// collision would route a client's answer to the wrong waiter - which fails
/// closed, but silently and in the wrong direction. The second question is read
/// by index rather than as "the latest frame", because reading it as the latest
/// would hand back the first question again and answer an id nobody is waiting
/// on.
#[tokio::test]
async fn a_bridge_request_carries_a_non_colliding_id() {
    let dir = TempDir::new().expect("temp dir");
    let client = Arc::new(Client::new(&dir));

    let mut ids = Vec::new();
    for round in 0..2 {
        let asking = {
            let client = Arc::clone(&client);
            tokio::spawn(async move {
                client
                    .bridge
                    .request_permission("s", "c", &client.out)
                    .await
            })
        };
        let asked = client.next_written(round).await;
        let id = asked["id"].as_str().expect("a text id").to_string();
        assert!(id.starts_with("tetanus-acp-"), "{id}");
        ids.push(id);

        client
            .reply(
                &asked["id"],
                json!({ "result": { "outcome": { "outcome": "cancelled" } } }),
            )
            .await;
        within("the permission answer", asking)
            .await
            .expect("joins");
    }

    assert_ne!(ids[0], ids[1], "each question gets its own id");
}

/// TC-PORT-ACP-15: a shut-down bridge releases every waiter and refuses further
/// work.
///
/// Input: a permission question outstanding when the connection closes, and a
/// call afterwards.
/// Expected: the question settles `Unavailable` rather than hanging, and the
/// later call is refused. A waiter nobody will ever answer is the one failure a
/// client cannot diagnose, because it looks exactly like an agent thinking -
/// which is why this case is bounded and why the bridge frees its waiters
/// rather than leaving them to a timeout.
#[tokio::test]
async fn shutdown_releases_waiters_and_refuses_later_calls() {
    let dir = TempDir::new().expect("temp dir");
    let client = Arc::new(Client::new(&dir));
    client.initialize().await;

    let asking = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            client
                .bridge
                .request_permission("s", "c", &client.out)
                .await
        })
    };
    client.next_written(0).await;

    client.bridge.shutdown().await;
    assert_eq!(
        within("the abandoned permission question", asking)
            .await
            .expect("joins"),
        ApprovalOutcome::Unavailable,
        "nobody is left to answer, so nobody granted",
    );

    let refused = client
        .call(
            method::SESSION_NEW,
            json!({ "cwd": "/tmp", "mcpServers": [] }),
        )
        .await;
    assert!(
        error_of(&refused)["message"]
            .as_str()
            .expect("a message")
            .contains("shut down"),
        "{refused}",
    );
}

/// TC-PORT-ACP-16: a malformed frame is still answered, and an unknown method
/// is refused rather than ignored.
///
/// Input: a frame that is not JSON, a frame that is JSON but not a message, an
/// unknown method, and an unknown notification.
/// Expected: the first two are answered with `id: null` - a client that is
/// waiting has to be released - the unknown method is `MethodNotFound`, and the
/// unknown notification is silently ignored so a client may speak a later minor
/// version.
#[tokio::test]
async fn malformed_frames_are_answered_and_unknown_notifications_ignored() {
    let dir = TempDir::new().expect("temp dir");
    let client = Client::new(&dir);
    client.initialize().await;

    for (raw, code) in [
        ("{not json", ErrorCode::ParseError),
        ("42", ErrorCode::InvalidRequest),
    ] {
        let answered = within("a malformed frame", client.bridge.frame(raw, &client.out))
            .await
            .expect("answered");
        let answered: Value = serde_json::from_str(&answered).expect("JSON");
        assert_eq!(answered["id"], Value::Null, "released with a null id");
        assert_eq!(answered["error"]["code"], json!(code.code()));
    }

    let unknown = client.call("session/load", json!({})).await;
    assert_eq!(
        error_of(&unknown)["code"],
        json!(ErrorCode::MethodNotFound.code()),
    );

    client.notify("session/hologram", json!({})).await;
}
