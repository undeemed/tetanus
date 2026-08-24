//! Test Design Specification: the parallel tool cap a document configures.
//!
//! Feature under test: `agent.max_parallel_tool_calls`, the setting that says
//! how many parallel-safe tool calls of one step may be in flight at once, and
//! the wiring that carries it from the document to a live turn.
//!
//! Approach: every case reads a real document off disk, because the key is
//! only correct if the reader's flattening produces it from the nesting a
//! reader would write. The last case runs a real turn whose step asks for two
//! parallel-safe calls, and watches how many of them were ever in flight
//! together, so what is asserted is the cap the engine actually ran under.
//!
//! Features NOT tested here: the pool rule itself - replenishment, barriers,
//! and results committed in model order. That is `tetanus_turn`, pinned by
//! TC-PORT-TOOL-1..5 in `crates/turn/tests/upstream_tool_calls.rs`, and it is
//! not restated.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tetanus_config::Config;
use tetanus_engine::agent::Providers;
use tetanus_engine::{boot, catalog, EngineConfig, HarnessEngine};
use tetanus_protocol::methods::{AgentPromptParams, Engine, SessionCreateParams};
use tetanus_protocol::types::ConfigLayer;
use tetanus_turn::engine::DEFAULT_MAX_PARALLEL_TOOL_CALLS;
use tetanus_turn::llm::{ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse};
use tetanus_turn::tools::{
    Tool, ToolCall, ToolError, ToolMode, ToolOutcome, ToolRegistry, ToolSchema,
};

/// TC-PARALLEL-1: a build with no document runs the compiled cap, and
/// publishes the key that would change it.
///
/// Input: a document that does not exist.
/// Expected: the cap is `DEFAULT_MAX_PARALLEL_TOOL_CALLS`, and
/// `agent.max_parallel_tool_calls` appears in `config.dump` at the `Default`
/// layer with that value. A key the engine reads but never publishes is a
/// setting nobody could discover.
#[tokio::test]
async fn no_document_is_the_compiled_cap_and_it_is_visible() {
    let dir = TempDir::new().expect("temp dir");
    let resolved = boot::document(&dir.path().join("absent.yaml")).expect("no document");

    let config = EngineConfig::from_settings(resolved).expect("defaults");
    assert_eq!(
        config.max_parallel_tool_calls,
        DEFAULT_MAX_PARALLEL_TOOL_CALLS
    );

    let entry = dumped(&HarnessEngine::new(config)).await;
    assert_eq!(
        entry.value,
        serde_json::json!(DEFAULT_MAX_PARALLEL_TOOL_CALLS.get())
    );
    assert_eq!(entry.layer, ConfigLayer::Default);
}

/// TC-PARALLEL-2: a document that names a cap gets one, and says where it came
/// from.
///
/// Input: a document setting `agent.max_parallel_tool_calls` to 1.
/// Expected: the resolved cap is 1, and the dump reports the value at the
/// `File` layer rather than the compiled default.
#[tokio::test]
async fn a_document_names_the_cap_and_the_dump_says_so() {
    let dir = TempDir::new().expect("temp dir");
    let resolved = settings(dir.path(), r#"{"agent": {"max_parallel_tool_calls": 1}}"#);

    let config = EngineConfig::from_settings(resolved).expect("resolve");
    assert_eq!(config.max_parallel_tool_calls.get(), 1);

    let entry = dumped(&HarnessEngine::new(config)).await;
    assert_eq!(entry.value, serde_json::json!(1));
    assert_eq!(entry.layer, ConfigLayer::File);
}

/// TC-PARALLEL-3: a cap of none is refused, and the message says what a cap is.
///
/// A cap of one is serial dispatch, which is a thing to ask for. Zero is a pool
/// that can start nothing, so a step with a tool call would never finish; it is
/// a mistake to report, not a limit to honour.
///
/// Input: a document setting the cap to 0.
/// Expected: `ConfigError::BadValue` naming the key, the expectation and the
/// value found, and no engine built.
#[test]
fn a_cap_of_none_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let resolved = settings(dir.path(), r#"{"agent": {"max_parallel_tool_calls": 0}}"#);

    let error = EngineConfig::from_settings(resolved)
        .err()
        .expect("zero is not a cap");
    let said = error.to_string();
    assert!(
        said.contains(catalog::key::MAX_PARALLEL_TOOL_CALLS),
        "{said}"
    );
    assert!(said.contains("one or more"), "{said}");
    assert!(said.contains('0'), "{said}");
}

/// TC-PARALLEL-4: a value that is not a whole number of calls is refused as
/// the document is read.
///
/// Input: a document setting the cap to the text `"two"`.
/// Expected: `ConfigError::BadValue` naming the key, from the read rather than
/// from `from_settings`. The key is declared a whole number
/// (`crates/engine/src/boot.rs`), so the shape is judged before an engine
/// exists; TC-PARALLEL-3 stays where it was, because zero is a whole number and
/// only the reader that wants a cap knows it is useless. Either way the value is
/// not ignored: ignoring it would run the engine on a setting the user did not
/// write.
#[test]
fn a_cap_that_is_not_a_number_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, r#"{"agent": {"max_parallel_tool_calls": "two"}}"#).expect("write");

    let error = boot::document(&path).expect_err("text is not a cap");

    assert!(
        error
            .to_string()
            .contains(catalog::key::MAX_PARALLEL_TOOL_CALLS),
        "{error}"
    );
    assert!(error.to_string().contains("a whole number"), "{error}");
}

/// TC-PARALLEL-6: where the document and the engine differ, the dump reports
/// the cap the engine runs, with the document's provenance.
///
/// The rule is TC-CAT-6's, restated for this key: a surface that printed the
/// document's value would tell the user a step will overlap by a number it
/// will not. A composer in Rust is allowed to override a resolved value, and
/// what it settled is what the turn uses.
///
/// Input: a document naming a cap of 1, and an `EngineConfig` built over it
/// with a cap of 3.
/// Expected: the dump reads 3 at the `File` layer - the engine's value, the
/// document's provenance.
#[tokio::test]
async fn the_dump_reports_the_cap_the_engine_runs() {
    let dir = TempDir::new().expect("temp dir");
    let resolved = settings(dir.path(), r#"{"agent": {"max_parallel_tool_calls": 1}}"#);

    let engine = HarnessEngine::new(EngineConfig {
        max_parallel_tool_calls: NonZeroUsize::new(3).expect("a cap"),
        ..EngineConfig::from_settings(resolved).expect("resolve")
    });

    let entry = dumped(&engine).await;
    assert_eq!(entry.value, serde_json::json!(3), "the engine's cap");
    assert_eq!(entry.layer, ConfigLayer::File, "the document's provenance");
}

/// TC-PARALLEL-5: the cap a document names is the cap a live turn runs under.
///
/// Input: a document capping the engine at one call, a registry holding one
/// parallel-safe tool that reports how many copies of itself were running, and
/// one prompt whose first step asks for two calls of it.
/// Expected: both calls ran, and never together. The tool yields while it is in
/// flight, so an uncapped pool would have started the second before the first
/// finished; a peak of one is the document's cap and nothing else.
#[tokio::test]
async fn the_document_cap_reaches_a_live_turn() {
    let dir = TempDir::new().expect("temp dir");
    let watch = Watch::default();
    let resolved = settings(dir.path(), r#"{"agent": {"max_parallel_tool_calls": 1}}"#);

    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        tools: Arc::new(ToolRegistry::new().with(Arc::new(Twin(watch.clone())))),
        providers: Arc::new(One(Arc::new(AsksForTwo::default()))),
        ..EngineConfig::from_settings(resolved).expect("resolve")
    });

    let session = engine
        .session_create(SessionCreateParams::default())
        .await
        .expect("create")
        .session_id;
    engine
        .agent_prompt(AgentPromptParams {
            session_id: session,
            content: "run both".into(),
        })
        .await
        .expect("prompt");

    assert_eq!(watch.ran.load(Ordering::SeqCst), 2, "both calls ran");
    assert_eq!(
        watch.peak.load(Ordering::SeqCst),
        1,
        "the cap the document named is the overlap the turn allowed"
    );
}

/// Write `text` as the settings document, and resolve it.
fn settings(dir: &Path, text: &str) -> Config {
    let path = dir.join("settings.json");
    std::fs::write(&path, text).expect("write");
    boot::document(&path).expect("read")
}

/// The `agent.max_parallel_tool_calls` entry of a running engine's dump.
async fn dumped(engine: &HarnessEngine) -> tetanus_protocol::types::ConfigEntry {
    engine
        .config_dump()
        .await
        .expect("dump")
        .entries
        .into_iter()
        .find(|entry| entry.key == catalog::key::MAX_PARALLEL_TOOL_CALLS)
        .expect("the key is in the dump")
}

/// How many copies of one tool ran, and how many ever ran together.
#[derive(Clone, Default)]
struct Watch {
    live: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    ran: Arc<AtomicUsize>,
}

/// A parallel-safe tool that yields while it holds its slot, so two calls of it
/// overlap whenever the pool is allowed to start both.
struct Twin(Watch);

#[async_trait::async_trait]
impl Tool for Twin {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "twin".into(),
            description: "Run beside a copy of itself.".into(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    fn mode(&self, _arguments: &serde_json::Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, _arguments: &serde_json::Value) -> Result<ToolOutcome, ToolError> {
        let live = self.0.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.0.peak.fetch_max(live, Ordering::SeqCst);
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        self.0.live.fetch_sub(1, Ordering::SeqCst);
        self.0.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutcome::ok("twin"))
    }
}

/// An adapter whose first answer asks for two calls of one parallel-safe tool,
/// and whose next answer ends the turn.
#[derive(Default)]
struct AsksForTwo {
    asked: Mutex<bool>,
}

#[async_trait::async_trait]
impl LlmAdapter for AsksForTwo {
    fn provider(&self) -> &str {
        tetanus_turn::llm::mock::PROVIDER
    }

    fn models(&self) -> Vec<String> {
        vec![tetanus_turn::llm::mock::MODEL.to_string()]
    }

    async fn stream(
        &self,
        _request: &ModelRequest,
        _sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError> {
        let first = !std::mem::replace(&mut *self.asked.lock().expect("asked"), true);
        Ok(ModelResponse {
            content: if first { String::new() } else { "done".into() },
            tool_calls: if first {
                vec![twin("one"), twin("two")]
            } else {
                Vec::new()
            },
            finish_reason: "stop".into(),
            ..Default::default()
        })
    }
}

fn twin(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: "twin".into(),
        arguments: serde_json::json!({}),
    }
}

/// One adapter is the whole catalog.
struct One(Arc<dyn LlmAdapter>);

impl Providers for One {
    fn all(&self) -> Vec<Arc<dyn LlmAdapter>> {
        vec![Arc::clone(&self.0)]
    }
}
