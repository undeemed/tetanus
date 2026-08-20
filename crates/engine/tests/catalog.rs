//! Conformance for the read-only calls: `catalog.models`, `catalog.tools` and
//! `config.dump`.
//!
//! Test design: these calls exist so a surface never has to work out for
//! itself what the engine is running. Each case therefore asserts the answer
//! against the engine it was built with, not against a constant.
//!
//! No case writes an environment variable. A test that did would change what a
//! test running beside it sees, so the credential cases use one variable every
//! environment has and one no environment has.

use std::sync::Arc;

use tempfile::TempDir;
use tetanus_config::{Config, Layer};
use tetanus_engine::agent::Providers;
use tetanus_engine::catalog::key;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::Engine;
use tetanus_protocol::types::{ConfigEntry, ConfigLayer, REDACTED};
use tetanus_turn::llm::{ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse};
use tetanus_turn::tools::{EchoTool, Tool, ToolRegistry};

/// An env var no environment sets, and one every environment sets.
const ABSENT: &str = "TETANUS_TEST_CREDENTIAL_THAT_IS_NEVER_SET";
const PRESENT: &str = "PATH";

/// A provider that exists only to be listed.
struct Stub {
    provider: &'static str,
    models: Vec<String>,
    credential_env: Option<&'static str>,
}

#[async_trait::async_trait]
impl LlmAdapter for Stub {
    fn provider(&self) -> &str {
        self.provider
    }
    fn models(&self) -> Vec<String> {
        self.models.clone()
    }
    fn credential_env(&self) -> Option<&str> {
        self.credential_env
    }
    async fn stream(
        &self,
        _request: &ModelRequest,
        _sink: &mut dyn ChunkSink,
    ) -> Result<ModelResponse, LlmError> {
        unreachable!("a catalog never runs a turn")
    }
}

struct Stubs(Vec<Arc<dyn LlmAdapter>>);

impl Providers for Stubs {
    fn all(&self) -> Vec<Arc<dyn LlmAdapter>> {
        self.0.clone()
    }
}

fn engine(config: EngineConfig) -> (HarnessEngine, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        ..config
    });
    (engine, dir)
}

/// TC-CAT-1: contract section 5, `ProviderDescriptor.available`. A provider
/// whose credential is absent is listed as unavailable and names the variable
/// it wants, so a picker greys the entry out instead of offering it and meeting
/// `MissingCredential` on the first turn.
#[tokio::test]
async fn a_provider_without_its_credential_is_listed_unavailable() {
    assert!(
        std::env::var(ABSENT).is_err(),
        "this case needs `{ABSENT}` to be unset"
    );
    assert!(
        !std::env::var(PRESENT).unwrap_or_default().is_empty(),
        "this case needs an environment with `{PRESENT}` set"
    );

    let (engine, _dir) = engine(EngineConfig {
        providers: Arc::new(Stubs(vec![
            Arc::new(Stub {
                provider: "keyless",
                models: vec!["keyless-1".into()],
                credential_env: None,
            }),
            Arc::new(Stub {
                provider: "unconfigured",
                models: vec!["remote-1".into(), "remote-2".into()],
                credential_env: Some(ABSENT),
            }),
            Arc::new(Stub {
                provider: "configured",
                models: Vec::new(),
                credential_env: Some(PRESENT),
            }),
        ])),
        ..EngineConfig::default()
    });

    let listed = engine.catalog_models().await.expect("models").providers;
    assert_eq!(
        listed
            .iter()
            .map(|provider| provider.provider.as_str())
            .collect::<Vec<_>>(),
        vec!["keyless", "unconfigured", "configured"],
        "providers are listed in the order the build registered them"
    );

    assert_eq!(listed[0].credential_env, None);
    assert!(listed[0].available, "a provider needing no key is usable");
    assert_eq!(listed[0].models, vec!["keyless-1".to_string()]);

    assert_eq!(listed[1].credential_env.as_deref(), Some(ABSENT));
    assert!(!listed[1].available, "its credential is absent");
    assert_eq!(
        listed[1].models.len(),
        2,
        "an unavailable provider still advertises its models"
    );

    assert_eq!(listed[2].credential_env.as_deref(), Some(PRESENT));
    assert!(listed[2].available, "its credential is present");
}

/// TC-CAT-2: the default build lists the offline provider as available, which
/// is what makes a model picker useful before any key is configured.
#[tokio::test]
async fn the_default_build_lists_the_offline_provider() {
    let (engine, _dir) = engine(EngineConfig::default());
    let listed = engine.catalog_models().await.expect("models").providers;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].provider, tetanus_turn::llm::mock::PROVIDER);
    assert_eq!(
        listed[0].models,
        vec![tetanus_turn::llm::mock::MODEL.to_string()]
    );
    assert_eq!(
        listed[0].credential_env, None,
        "the offline adapter needs no key"
    );
    assert!(listed[0].available);
}

/// TC-CAT-3: `catalog.tools` reports every tool the engine registered, with
/// the schema the model is offered. A help surface and the model therefore
/// read one list, and a tool cannot appear in help without being callable.
#[tokio::test]
async fn the_tool_catalog_is_the_registry_the_turn_runs() {
    let registry = Arc::new(ToolRegistry::new().with(Arc::new(EchoTool)));
    let (engine, _dir) = engine(EngineConfig {
        tools: Arc::clone(&registry),
        ..EngineConfig::default()
    });

    let listed = engine.catalog_tools().await.expect("tools").tools;
    let mut expected = registry.schemas();
    expected.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(listed.len(), expected.len());
    for (descriptor, schema) in listed.iter().zip(&expected) {
        assert_eq!(descriptor.name, schema.name);
        assert_eq!(descriptor.description, schema.description);
        assert_eq!(descriptor.parameters, schema.parameters);
    }

    let echo = EchoTool.schema();
    assert_eq!(listed[0].name, echo.name);
    assert_eq!(
        listed[0].parameters, echo.parameters,
        "the catalog carries the JSON Schema, not a summary of it"
    );
}

/// TC-CAT-4: an empty registry lists nothing. A build with no tools is a build
/// with no tools: not an error, and not a default list.
#[tokio::test]
async fn a_build_with_no_tools_lists_none() {
    let (engine, _dir) = engine(EngineConfig {
        tools: Arc::new(ToolRegistry::new()),
        ..EngineConfig::default()
    });
    assert!(engine
        .catalog_tools()
        .await
        .expect("tools")
        .tools
        .is_empty());
}

fn entry(entries: &[ConfigEntry], key: &str) -> ConfigEntry {
    entries
        .iter()
        .find(|entry| entry.key == key)
        .unwrap_or_else(|| panic!("`{key}` is missing from the dump"))
        .clone()
}

/// TC-CAT-5: `config.dump` reports the value the engine will actually use, and
/// the layer the caller resolved it from. A key the caller never named is
/// reported at the `default` layer rather than omitted, so a config surface
/// shows the whole effective configuration.
#[tokio::test]
async fn the_dump_reports_effective_values_with_their_provenance() {
    let mut resolved = Config::default();
    resolved.set(key::PROVIDER, serde_json::json!("mock"), Layer::Flag);
    resolved.set(key::MAX_STEPS, serde_json::json!(3), Layer::File);

    let dir = TempDir::new().expect("temp dir");
    let engine = HarnessEngine::new(EngineConfig {
        sessions_root: dir.path().to_path_buf(),
        default_provider: "mock".into(),
        default_model: "mock-echo-1".into(),
        max_steps: 3,
        resolved: Arc::new(resolved),
        ..EngineConfig::default()
    });

    let entries = engine.config_dump().await.expect("dump").entries;
    assert_eq!(
        entry(&entries, key::PROVIDER).value,
        serde_json::json!("mock")
    );
    assert_eq!(entry(&entries, key::PROVIDER).layer, ConfigLayer::Flag);
    assert_eq!(entry(&entries, key::MAX_STEPS).value, serde_json::json!(3));
    assert_eq!(entry(&entries, key::MAX_STEPS).layer, ConfigLayer::File);

    assert_eq!(
        entry(&entries, key::MODEL).value,
        serde_json::json!("mock-echo-1")
    );
    assert_eq!(
        entry(&entries, key::MODEL).layer,
        ConfigLayer::Default,
        "a key the caller never named came from the default"
    );
    assert_eq!(
        entry(&entries, key::SESSIONS_ROOT).value,
        serde_json::json!(dir.path().display().to_string()),
        "the journal root is where journals are actually written"
    );

    let keys: Vec<&str> = entries.iter().map(|entry| entry.key.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted, "entries are ordered by key");
}

/// TC-CAT-6: where the caller's resolved value and the engine's differ, the
/// dump reports the engine's. A surface that printed the caller's would tell
/// the user a turn will use a model it will not use. A key the engine does not
/// settle passes through with the caller's own value and layer.
#[tokio::test]
async fn the_dump_reports_the_engine_not_the_callers_copy() {
    let mut resolved = Config::default();
    resolved.set(key::MODEL, serde_json::json!("stale-model"), Layer::Env);
    resolved.set(
        key::SESSIONS_ROOT,
        serde_json::json!("/nowhere"),
        Layer::Env,
    );
    resolved.set("ui.theme", serde_json::json!("dark"), Layer::File);

    let (engine, dir) = engine(EngineConfig {
        default_model: "running-model".into(),
        resolved: Arc::new(resolved),
        ..EngineConfig::default()
    });

    let entries = engine.config_dump().await.expect("dump").entries;
    assert_eq!(
        entry(&entries, key::MODEL).value,
        serde_json::json!("running-model")
    );
    assert_eq!(
        entry(&entries, key::MODEL).layer,
        ConfigLayer::Env,
        "the value is the engine's, the provenance is the caller's"
    );
    assert_eq!(
        entry(&entries, key::SESSIONS_ROOT).value,
        serde_json::json!(dir.path().display().to_string())
    );
    assert_eq!(entry(&entries, "ui.theme").value, serde_json::json!("dark"));
    assert_eq!(entry(&entries, "ui.theme").layer, ConfigLayer::File);
}

/// TC-CFG-SECRET-1: contract section 4.3. A key whose name says it holds a
/// credential is dumped without its value, and with everything else it had.
///
/// Input: a document holding an API key, resolved at the file layer.
/// Expected: the entry is in the dump, its value is the published sentinel,
/// and its key and layer are what the caller resolved. A surface can still
/// tell the user the key is set and which layer set it, which is what it needs
/// to say for the user to find the value in the file they wrote it in.
#[tokio::test]
async fn a_secret_keeps_its_entry_and_loses_its_value() {
    let mut resolved = Config::default();
    resolved.set(
        "llm.providers.deepseek.api_key",
        serde_json::json!("sk-live-must-not-be-published"),
        Layer::File,
    );

    let (engine, _dir) = engine(EngineConfig {
        resolved: Arc::new(resolved),
        ..EngineConfig::default()
    });

    let entries = engine.config_dump().await.expect("dump").entries;
    let secret = entry(&entries, "llm.providers.deepseek.api_key");
    assert_eq!(secret.value, serde_json::json!(REDACTED));
    assert_eq!(secret.layer, ConfigLayer::File);
}

/// TC-CFG-SECRET-2: the value is nowhere in the answer, in any form.
///
/// Input: the same document, with a credential in three shapes a document
/// writes them - the snake case name, the camel case name, and a bearer token
/// under another provider.
/// Expected: the serialized dump does not contain any of the three values.
/// TC-CFG-SECRET-1 reads one field; this reads the whole frame, because a
/// value that survives anywhere in it has been published, and it is the frame
/// and not the field that goes to the carrier.
#[tokio::test]
async fn no_secret_survives_anywhere_in_the_dump() {
    let mut resolved = Config::default();
    for (key, value) in [
        ("llm.providers.deepseek.api_key", "sk-snake"),
        ("llm.providers.acme.apiKey", "sk-camel"),
        ("llm.providers.acme.auth_token", "bearer-token"),
    ] {
        resolved.set(key, serde_json::json!(value), Layer::File);
    }

    let (engine, _dir) = engine(EngineConfig {
        resolved: Arc::new(resolved),
        ..EngineConfig::default()
    });

    let dump = engine.config_dump().await.expect("dump");
    let frame = serde_json::to_string(&dump).expect("serialize");
    for value in ["sk-snake", "sk-camel", "bearer-token"] {
        assert!(
            !frame.contains(value),
            "`{value}` reached the carrier in `{frame}`"
        );
    }
    assert_eq!(
        frame.matches(REDACTED).count(),
        3,
        "each withheld value leaves its entry behind"
    );
}

/// TC-CFG-SECRET-3: a key that only mentions a credential is published whole.
///
/// Input: the environment variable a key is read from, beside the key itself.
/// Expected: the variable's name is dumped as the caller resolved it, and only
/// the key is withheld. This is the case that costs a user something when it
/// fails the other way: `api_key_env` is how they find out which variable to
/// set, and a dump that hides it hides the answer to the question they opened
/// it with.
#[tokio::test]
async fn a_key_that_only_mentions_a_credential_is_published() {
    let mut resolved = Config::default();
    resolved.set(
        "llm.providers.deepseek.api_key_env",
        serde_json::json!("DEEPSEEK_API_KEY"),
        Layer::File,
    );
    resolved.set(
        "llm.providers.deepseek.api_key",
        serde_json::json!("sk-live"),
        Layer::File,
    );

    let (engine, _dir) = engine(EngineConfig {
        resolved: Arc::new(resolved),
        ..EngineConfig::default()
    });

    let entries = engine.config_dump().await.expect("dump").entries;
    assert_eq!(
        entry(&entries, "llm.providers.deepseek.api_key_env").value,
        serde_json::json!("DEEPSEEK_API_KEY")
    );
    assert_eq!(
        entry(&entries, "llm.providers.deepseek.api_key").value,
        serde_json::json!(REDACTED)
    );
}

/// TC-CFG-SECRET-4: the layer a secret came from does not change the answer.
///
/// Input: one credential per layer above the default - file, environment and
/// flag.
/// Expected: all three are withheld, and each still reports its own layer. The
/// rule is about the key, not about where the value came from: a credential
/// passed on the command line is as published by a dump as one written in the
/// document, and a surface still has to be able to say which one it is
/// reading.
#[tokio::test]
async fn a_secret_is_withheld_whatever_layer_set_it() {
    let mut resolved = Config::default();
    resolved.set("a.api_key", serde_json::json!("from-file"), Layer::File);
    resolved.set("b.api_key", serde_json::json!("from-env"), Layer::Env);
    resolved.set("c.api_key", serde_json::json!("from-flag"), Layer::Flag);

    let (engine, _dir) = engine(EngineConfig {
        resolved: Arc::new(resolved),
        ..EngineConfig::default()
    });

    let entries = engine.config_dump().await.expect("dump").entries;
    for (key, layer) in [
        ("a.api_key", ConfigLayer::File),
        ("b.api_key", ConfigLayer::Env),
        ("c.api_key", ConfigLayer::Flag),
    ] {
        let withheld = entry(&entries, key);
        assert_eq!(withheld.value, serde_json::json!(REDACTED), "`{key}`");
        assert_eq!(withheld.layer, layer, "`{key}`");
    }
}
