//! What the MCP cases talk to: a scripted server on a channel pair, and the
//! real fixture program on a real pipe.
//!
//! Both exist on purpose. A channel pair pins what the client does with a
//! *message* - an answer out of order, a line that is not JSON, a cursor that
//! repeats - with no timing in the case at all. The fixture program pins what
//! it does with a *process* - an exit status, a pipe that ends, a child that
//! will not leave. Neither substitutes for the other.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tetanus_mcp::memory::Peer;
use tetanus_mcp::{ClientInfo, Launcher, Link, McpClient, McpFault, ServerCommand, Timeouts};

/// The fixture server, in the named mode. `serve` is a correct server.
pub fn fixture(mode: &str) -> ServerCommand {
    ServerCommand::new(env!("CARGO_BIN_EXE_tetanus-mcp-fixture"))
        .env("TETANUS_MCP_FIXTURE", mode)
        // Short, because two cases wait for it and neither is about patience.
        .grace(Duration::from_millis(300))
}

/// Connect to the fixture server in the named mode.
pub async fn connect_fixture(
    mode: &str,
    timeouts: Timeouts,
) -> Result<McpClient, tetanus_mcp::McpFault> {
    let link = fixture(mode).spawn().expect("the fixture server starts");
    McpClient::connect("fixture", link, timeouts, ClientInfo::default()).await
}

/// Budgets short enough that a case about a server that never answers finishes
/// in well under a second.
pub fn brisk() -> Timeouts {
    Timeouts {
        handshake: Duration::from_millis(400),
        request: Duration::from_millis(400),
    }
}

/// Drive a scripted server on the other end of a channel pair.
///
/// `answer` is handed every message the client sent and returns the lines to
/// write back, so a case can answer out of order, answer twice, or answer with
/// something that is not a message at all.
pub fn scripted<F>(mut peer: Peer, mut answer: F) -> tokio::task::JoinHandle<()>
where
    F: FnMut(&Value) -> Vec<String> + Send + 'static,
{
    tokio::spawn(async move {
        while let Some(line) = peer.recv().await {
            let message: Value = serde_json::from_str(&line).expect("the client sends JSON");
            for reply in answer(&message) {
                if !peer.send(reply) {
                    return;
                }
            }
        }
    })
}

/// The handshake answer a correct server gives.
pub fn hello(id: &Value) -> String {
    result(
        id,
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": { "listChanged": true } },
            "serverInfo": { "name": "scripted", "version": "9.9.9" },
        }),
    )
}

/// A `result` frame.
pub fn result(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

/// An `error` frame.
pub fn refusal(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

/// The id of a request the client sent.
pub fn id_of(message: &Value) -> Value {
    message.get("id").cloned().unwrap_or(Value::Null)
}

/// The method of a message the client sent.
pub fn method_of(message: &Value) -> String {
    message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Whether a process is still on this system. Used to prove no child is left
/// behind; `/proc` is the only reading that does not depend on this process
/// still being the parent.
#[cfg(target_os = "linux")]
pub fn process_exists(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// A correct in-memory server that can be told to go away, so a case can lose
/// a connection without spending a process on it.
pub struct FakeServer {
    hangup: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FakeServer {
    /// Drop the peer, which the client reads as end of stream - the same thing
    /// a server exiting looks like from this side.
    pub fn hang_up(&mut self) {
        if let Some(hangup) = self.hangup.take() {
            let _ = hangup.send(());
        }
    }
}

/// A server advertising `tools`, answering every call with the tool's name.
pub fn fake_server(tools: Vec<String>) -> (Link, FakeServer) {
    let (link, mut peer) = tetanus_mcp::memory::pair();
    let (hangup, mut told) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut told => return,
                line = peer.recv() => {
                    let Some(line) = line else { return };
                    let message: Value = serde_json::from_str(&line).expect("the client sends JSON");
                    let id = id_of(&message);
                    match method_of(&message).as_str() {
                        m if m == tetanus_mcp::wire::method::INITIALIZE => { peer.send(hello(&id)); }
                        m if m == tetanus_mcp::wire::method::TOOLS_LIST => {
                            let listed: Vec<Value> = tools
                                .iter()
                                .map(|name| json!({ "name": name, "description": "a fake tool" }))
                                .collect();
                            peer.send(result(&id, json!({ "tools": listed })));
                        }
                        m if m == tetanus_mcp::wire::method::TOOLS_CALL => {
                            let name = message
                                .pointer("/params/name")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            peer.send(result(
                                &id,
                                json!({ "content": [{ "type": "text", "text": format!("ran {name}") }] }),
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
    });
    (
        link,
        FakeServer {
            hangup: Some(hangup),
        },
    )
}

/// A launcher a case scripts: it hands over the link the plan names for each
/// launch, in order, and fails the launch when the plan says nothing.
pub struct ScriptedLauncher {
    plan: Mutex<Box<dyn FnMut(u32) -> Option<Link> + Send>>,
    launches: AtomicU32,
}

impl ScriptedLauncher {
    /// `plan` is called with the 1-based launch number.
    pub fn new(plan: impl FnMut(u32) -> Option<Link> + Send + 'static) -> Arc<Self> {
        Arc::new(Self {
            plan: Mutex::new(Box::new(plan)),
            launches: AtomicU32::new(0),
        })
    }

    pub fn launches(&self) -> u32 {
        self.launches.load(Ordering::Acquire)
    }
}

#[async_trait::async_trait]
impl Launcher for ScriptedLauncher {
    async fn launch(&self) -> Result<Link, McpFault> {
        let attempt = self.launches.fetch_add(1, Ordering::AcqRel) + 1;
        let made = { (self.plan.lock().expect("plan"))(attempt) };
        made.ok_or_else(|| McpFault::Transport(format!("launch {attempt} was scripted to fail")))
    }
}

/// A model that asks for one named tool on its first step and answers with
/// whatever came back on its second.
///
/// `tetanus_turn::llm::mock` always asks for `echo`, so a turn about an MCP
/// tool needs a model that asks for that tool by name. Everything else about
/// the shape - one step that calls, one that answers - is the mock's, so the
/// documented event sequence is the documented event sequence.
pub struct ModelAsking {
    pub tool: String,
    pub arguments: Value,
}

#[async_trait::async_trait]
impl tetanus_turn::llm::LlmAdapter for ModelAsking {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn models(&self) -> Vec<String> {
        vec!["scripted-1".to_string()]
    }

    async fn stream(
        &self,
        request: &tetanus_turn::llm::ModelRequest,
        sink: &mut dyn tetanus_turn::llm::ChunkSink,
    ) -> Result<tetanus_turn::llm::ModelResponse, tetanus_turn::llm::LlmError> {
        use tetanus_turn::llm::{ModelResponse, Role, StreamChunk};

        let answered = request
            .messages
            .last()
            .filter(|message| message.role == Role::Tool);
        if let Some(answer) = answered {
            let content = format!("the tool said: {}", answer.content);
            sink.chunk(StreamChunk::Text {
                delta: content.clone(),
            })
            .await?;
            return Ok(ModelResponse {
                content,
                reasoning: String::new(),
                tool_calls: Vec::new(),
                finish_reason: "stop".into(),
                usage: None,
            });
        }

        let content = format!("I will use {}.", self.tool);
        sink.chunk(StreamChunk::Text {
            delta: content.clone(),
        })
        .await?;
        let call = tetanus_turn::tools::ToolCall {
            id: "call_1".into(),
            name: self.tool.clone(),
            arguments: self.arguments.clone(),
        };
        sink.chunk(StreamChunk::ToolCall { call: call.clone() })
            .await?;
        Ok(ModelResponse {
            content,
            reasoning: String::new(),
            tool_calls: vec![call],
            finish_reason: "tool_calls".into(),
            usage: None,
        })
    }
}

/// One booted turn engine over a caller-supplied registry and model, writing
/// to a temporary journal.
pub struct TurnFixture {
    pub engine: tetanus_turn::TurnEngine,
    pub log: std::sync::Arc<dyn tetanus_session::SessionLog>,
    _dir: tempfile::TempDir,
}

impl TurnFixture {
    pub fn new(
        name: &str,
        tools: tetanus_turn::tools::ToolRegistry,
        model: Arc<dyn tetanus_turn::llm::LlmAdapter>,
    ) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let bus = tetanus_core::EventBus::new();
        let log: std::sync::Arc<dyn tetanus_session::SessionLog> =
            tetanus_session::JsonlSessionLog::create(
                name,
                dir.path().join("turn.jsonl"),
                bus.clone(),
            )
            .expect("journal");
        let ctx =
            tetanus_turn::boot::boot(bus, model, Arc::new(tools), Arc::clone(&log)).expect("boot");
        let engine = tetanus_turn::TurnEngine::from_context(
            &ctx,
            tetanus_turn::TurnConfig {
                model: "scripted-1".to_string(),
                ..tetanus_turn::TurnConfig::default()
            },
        )
        .expect("engine");
        Self {
            engine,
            log,
            _dir: dir,
        }
    }

    /// The `tool/result` records this journal holds, as (name, ok, content).
    pub fn tool_results(&self) -> Vec<(String, bool, String)> {
        self.log
            .events()
            .iter()
            .filter(|event| event.ty == "tool/result")
            .map(|event| {
                (
                    event.data["name"].as_str().unwrap_or_default().to_string(),
                    event.data["ok"].as_bool().unwrap_or_default(),
                    event.data["content"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect()
    }
}

/// Wait for a condition to become true, polling briefly. Every case that uses
/// it is waiting on a supervisor task, whose timing is the operating system's
/// and not the case's; the bound is what turns a hang into a failure.
pub async fn eventually(within: Duration, mut settled: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        if settled() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    settled()
}
