//! Test Design Specification: the tool order a document configures.
//!
//! Feature under test: [`tetanus_engine::tools::order`], which turns the
//! settings document into the order the engine will offer its tools in, and
//! the wiring that carries it to a live turn.
//!
//! Approach: every case reads a real document off disk, because the key is
//! only correct if the reader's flattening produces it from the nesting a
//! reader would write. The last case runs a real turn against a recording
//! adapter, so what is asserted is the list the model was actually offered.
//!
//! Features NOT tested here: the order rule itself - the rest entry, duplicate
//! names, unregistered names, and the arrangement a registry then produces.
//! That is `tetanus_turn::tools::ToolOrder`, pinned by TC-PORT-LOOP-9..13 in
//! `crates/turn/tests/upstream_loop.rs`, and it is not restated.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::Path;
use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tetanus_config::Config;
use tetanus_engine::agent::Providers;
use tetanus_engine::{boot, tools, EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{AgentPromptParams, Engine, SessionCreateParams};
use tetanus_protocol::types::ConfigLayer;
use tetanus_turn::llm::{mock, ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse};
use tetanus_turn::tools::{
    EchoTool, Tool, ToolError, ToolOutcome, ToolRegistry, ToolSchema, TOOL_ORDER_REST,
};

/// TC-ORDER-1: a build with no document reads its tools in the canonical
/// order, and publishes the key that would change it.
///
/// Input: a document that does not exist.
/// Expected: no order is resolved, and `tools.order` appears in `config.dump`
/// at the `Default` layer as the empty list. A key the engine reads but never
/// publishes is a setting nobody could discover.
#[tokio::test]
async fn no_document_is_the_canonical_order_and_it_is_visible() {
    let dir = TempDir::new().expect("temp dir");
    let resolved = boot::document(&dir.path().join("absent.yaml")).expect("no document");

    let config = EngineConfig::from_settings(resolved).expect("defaults");
    assert!(config.tool_order.is_none(), "nothing named an order");

    let engine = HarnessEngine::new(config);
    let entry = dumped(&engine).await;
    assert_eq!(entry.value, serde_json::json!([]));
    assert_eq!(entry.layer, ConfigLayer::Default);
}

/// TC-ORDER-2: a document that names an order gets one, and says where it came
/// from.
///
/// Input: `tools: {order: ["echo", "<unlisted-tools>"]}`, against the default
/// registry.
/// Expected: an order is resolved, and `config.dump` reports the list at the
/// `File` layer.
#[tokio::test]
async fn a_document_names_the_order() {
    let dir = TempDir::new().expect("temp dir");
    let resolved = settings(
        dir.path(),
        &format!(r#"{{"tools": {{"order": ["echo", "{TOOL_ORDER_REST}"]}}}}"#),
    );

    let config = EngineConfig::from_settings(resolved).expect("resolve");
    assert!(config.tool_order.is_some(), "the document named an order");

    let engine = HarnessEngine::new(config);
    let entry = dumped(&engine).await;
    assert_eq!(entry.value, serde_json::json!(["echo", TOOL_ORDER_REST]));
    assert_eq!(entry.layer, ConfigLayer::File, "it came from the document");
}

/// TC-ORDER-3: an order the engine cannot run is refused, and the message says
/// which key was wrong.
///
/// Input: five documents - a name nobody registered, a list with no rest
/// entry, a name listed twice, a value that is not a list, and a list holding
/// something that is not a name.
/// Expected: each is `ConfigError::BadValue` whose message leads with
/// `tools.order`. An order silently dropped would offer the model a list its
/// author did not write.
#[test]
fn an_order_the_engine_cannot_run_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    for (case, document) in [
        (
            "a name nobody registered",
            format!(r#"{{"tools": {{"order": ["nope", "{TOOL_ORDER_REST}"]}}}}"#),
        ),
        ("no rest entry", r#"{"tools": {"order": ["echo"]}}"#.into()),
        (
            "a name listed twice",
            format!(r#"{{"tools": {{"order": ["echo", "echo", "{TOOL_ORDER_REST}"]}}}}"#),
        ),
        ("not a list", r#"{"tools": {"order": "echo"}}"#.into()),
        (
            "a list of something else",
            r#"{"tools": {"order": ["echo", 7]}}"#.into(),
        ),
    ] {
        let resolved = settings(dir.path(), &document);
        let error = EngineConfig::from_settings(resolved)
            .err()
            .unwrap_or_else(|| panic!("{case} is refused"));
        assert!(
            error.to_string().starts_with(tools::key::ORDER),
            "{case}: the message leads with the key: {error}"
        );
    }
}

/// TC-ORDER-4: an empty list is no order.
///
/// Input: `tools: {order: []}`, which is also the compiled default.
/// Expected: no order is resolved, and no failure. An order that arranges
/// nothing still needs the rest entry, so an empty list can only mean the
/// canonical order.
#[test]
fn an_empty_list_is_no_order() {
    let dir = TempDir::new().expect("temp dir");
    let resolved = settings(dir.path(), r#"{"tools": {"order": []}}"#);

    let config = EngineConfig::from_settings(resolved).expect("resolve");

    assert!(config.tool_order.is_none());
}

/// TC-ORDER-5: the order a document names is the order the model is offered.
///
/// Input: a two-tool registry, an order naming the second tool first, and one
/// prompt on a live session.
/// Expected: the model's request lists `ping` before `echo`, which is not the
/// canonical order the same registry produces unarranged.
#[tokio::test]
async fn the_document_order_reaches_the_model() {
    let dir = TempDir::new().expect("temp dir");
    let registry = Arc::new(
        ToolRegistry::new()
            .with(Arc::new(EchoTool))
            .with(Arc::new(Ping)),
    );
    let resolved = settings(
        dir.path(),
        &format!(r#"{{"tools": {{"order": ["ping", "{TOOL_ORDER_REST}"]}}}}"#),
    );
    // The registry an order is read against is the registry that will serve
    // it, which is why a caller with its own tools resolves the order itself.
    let order = tools::order(&resolved, &registry)
        .expect("read")
        .expect("an order");
    let offered = Arc::new(Mutex::new(Vec::new()));
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        tools: registry,
        tool_order: Some(order),
        providers: Arc::new(One(Arc::new(Recorder {
            inner: mock::MockAdapter::new(),
            offered: Arc::clone(&offered),
        }))),
        ..EngineConfig::default()
    });

    let session = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create")
        .session_id;
    engine
        .agent_prompt(AgentPromptParams {
            session_id: session,
            content: "hello".into(),
        })
        .await
        .expect("prompt");

    let seen = offered.lock().expect("offered").clone();
    assert_eq!(
        seen.first().map(Vec::len),
        Some(2),
        "both tools were offered"
    );
    assert_eq!(
        seen[0],
        ["ping", "echo"],
        "the document's order, not canonical"
    );
}

/// Write `text` as the settings document, and resolve it.
fn settings(dir: &Path, text: &str) -> Config {
    let path = dir.join("settings.json");
    std::fs::write(&path, text).expect("write");
    boot::document(&path).expect("read")
}

/// The `tools.order` entry of a running engine's `config.dump`.
async fn dumped(engine: &HarnessEngine) -> tetanus_protocol::types::ConfigEntry {
    engine
        .config_dump()
        .await
        .expect("dump")
        .entries
        .into_iter()
        .find(|entry| entry.key == tools::key::ORDER)
        .expect("`tools.order` is in the dump")
}

/// A second tool, so an order has something to rearrange.
struct Ping;

#[async_trait::async_trait]
impl Tool for Ping {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "ping".into(),
            description: "Answer `pong`.".into(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        Ok(ToolOutcome::ok("pong"))
    }
}

/// The offline mock, remembering the tool names each request carried.
struct Recorder {
    inner: mock::MockAdapter,
    offered: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl LlmAdapter for Recorder {
    fn provider(&self) -> &str {
        self.inner.provider()
    }

    fn models(&self) -> Vec<String> {
        self.inner.models()
    }

    async fn stream(
        &self,
        request: &ModelRequest,
        sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError> {
        self.offered
            .lock()
            .expect("offered")
            .push(request.tools.iter().map(|t| t.name.clone()).collect());
        self.inner.stream(request, sink).await
    }
}

struct One(Arc<dyn LlmAdapter>);

impl Providers for One {
    fn all(&self) -> Vec<Arc<dyn LlmAdapter>> {
        vec![Arc::clone(&self.0)]
    }
}
