//! Conformance for the read-only calls. This file starts with
//! `catalog.models`; `catalog.tools` and `config.dump` join it as they land.
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
use tetanus_engine::agent::Providers;
use tetanus_engine::{EngineConfig, HarnessEngine};
use tetanus_protocol::methods::Engine;
use tetanus_turn::llm::{ChunkSink, LlmAdapter, LlmError, ModelRequest, ModelResponse};

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
