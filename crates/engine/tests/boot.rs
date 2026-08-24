//! Conformance for the settings document the engine boots on.
//!
//! Test design: the layered config already has its own cases for reading a
//! document (`crates/config/tests`). These cover the step after it - what the
//! engine runs on, and what `config.dump` then reports - so each case names an
//! engine setting rather than a parse. Every case writes its document to a
//! temporary directory, so none reads a developer's real home.

use std::path::Path;

use tempfile::TempDir;
use tetanus_config::ConfigError;
use tetanus_engine::catalog::key;
use tetanus_engine::{boot, EngineConfig, HarnessEngine, DEFAULT_SESSIONS_ROOT};
use tetanus_protocol::methods::Engine;
use tetanus_protocol::types::ConfigLayer;

/// Write `text` as the settings document, and resolve it.
fn settings(dir: &Path, name: &str, text: &str) -> Result<tetanus_config::Config, ConfigError> {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("write");
    boot::document(&path)
}

/// TC-BOOT-1: with no document, every setting the engine settles is its
/// compiled default, and reports itself as one. A value that appeared from
/// nowhere is a value nobody can trace.
#[test]
fn no_document_leaves_the_compiled_defaults() {
    let dir = TempDir::new().expect("temp dir");
    let resolved = boot::document(&dir.path().join("settings.yaml")).expect("no document");

    let config = EngineConfig::from_settings(resolved).expect("defaults");
    let base = EngineConfig::default();
    assert_eq!(config.sessions_root, base.sessions_root);
    assert_eq!(config.default_provider, base.default_provider);
    assert_eq!(config.default_model, base.default_model);
    assert_eq!(config.max_steps, base.max_steps);

    for name in [
        key::SESSIONS_ROOT,
        key::PROVIDER,
        key::MODEL,
        key::MAX_STEPS,
    ] {
        let entry = config.resolved.get(name).expect("a default is a layer too");
        assert_eq!(entry.layer, tetanus_config::Layer::Default, "{name}");
    }
    assert_eq!(
        config.resolved.get(key::SESSIONS_ROOT).expect("root").value,
        serde_json::json!(DEFAULT_SESSIONS_ROOT)
    );
}

/// TC-BOOT-2: a document sets the engine's settings, and a key it leaves out
/// keeps the default rather than emptying.
#[test]
fn a_document_sets_what_it_names_and_no_more() {
    let dir = TempDir::new().expect("temp dir");
    let resolved = settings(
        dir.path(),
        "settings.yaml",
        "sessions:\n  root: /tmp/journals\nprovider:\n  default: deepseek\nagent:\n  max_steps: 32\n",
    )
    .expect("read");

    let config = EngineConfig::from_settings(resolved).expect("resolve");
    assert_eq!(config.sessions_root, Path::new("/tmp/journals"));
    assert_eq!(config.default_provider, "deepseek");
    assert_eq!(config.max_steps, 32);
    assert_eq!(
        config.default_model,
        EngineConfig::default().default_model,
        "a key the document leaves out keeps its default"
    );
}

/// TC-BOOT-3: a value the key does not take is refused, naming the key, at
/// whichever of the two stages can tell.
///
/// The stages are different questions and they are asked in order. Reading the
/// document checks it against the declared *shape*: a whole number written as
/// text is wrong whatever anything later does with it, and refusing it there
/// names the key before an engine exists. Resolving the settings then checks
/// what the value *means*: zero is an integer and an empty name is text, and
/// only the reader that wants them knows they are useless.
///
/// Running on a setting the user did not write is the failure both stages
/// exist to prevent, so each case asserts which stage caught it as well as
/// that it was caught.
#[test]
fn a_value_of_the_wrong_type_is_refused() {
    let dir = TempDir::new().expect("temp dir");

    // Wrong shape: the schema refuses these as the document is read.
    for (document, key) in [
        ("agent:\n  max_steps: many\n", key::MAX_STEPS),
        ("agent:\n  max_steps: 2.5\n", key::MAX_STEPS),
        ("provider:\n  default: 7\n", key::PROVIDER),
    ] {
        let error = settings(dir.path(), "settings.yaml", document)
            .err()
            .unwrap_or_else(|| panic!("`{document}` must be refused as it is read"));
        assert!(
            matches!(&error, ConfigError::BadValue { key: named, .. } if named == key),
            "`{document}` must name `{key}`, said: {error}"
        );
        assert!(
            error.to_string().starts_with(key),
            "the message leads with the key: {error}"
        );
    }

    // Right shape, useless value: the reader that wants it refuses these.
    for (document, key) in [
        ("agent:\n  max_steps: 0\n", key::MAX_STEPS),
        ("model:\n  default: '  '\n", key::MODEL),
    ] {
        let resolved = settings(dir.path(), "settings.yaml", document).expect("the shape is fine");
        let error = EngineConfig::from_settings(resolved)
            .err()
            .unwrap_or_else(|| panic!("`{document}` must be refused as it is resolved"));
        assert!(
            matches!(&error, ConfigError::BadValue { key: named, .. } if named == key),
            "`{document}` must name `{key}`, said: {error}"
        );
    }
}

/// TC-BOOT-3b: a scalar written where a section belongs is refused, and says
/// what to write instead.
///
/// Until the schema existed this was ignored: the write contributed the key
/// `llm`, which no reader claims, while every `llm.*` key went on resolving
/// from the layer below - so a user who thought they had turned retries off
/// had changed nothing and nothing said so. `TC-PORT-SET-5` pinned that
/// behaviour as the open question it was; it now pins the refusal.
#[test]
fn a_scalar_where_a_section_belongs_is_refused() {
    let dir = TempDir::new().expect("temp dir");

    let error = settings(dir.path(), "settings.yaml", "llm:\n  retry: off\n")
        .expect_err("a section cannot be set to a scalar");

    assert!(
        matches!(&error, ConfigError::SectionExpected { key, .. } if key == "llm.retry"),
        "said: {error}"
    );
    assert!(
        error.to_string().contains("Write the keys inside it"),
        "it says what to do instead: {error}"
    );
}

/// TC-BOOT-4: contract §4.7. What a document set reaches `config.dump` as the
/// value the engine will use, on the layer it came from, so a config surface
/// and the running engine cannot disagree.
#[tokio::test]
async fn the_document_reaches_config_dump() {
    let dir = TempDir::new().expect("temp dir");
    let resolved = settings(
        dir.path(),
        "settings.json",
        r#"{"agent": {"max_steps": 5}, "log": {"level": "debug"}}"#,
    )
    .expect("read");

    let engine = HarnessEngine::new(EngineConfig::from_settings(resolved).expect("resolve"));
    let dumped = engine.config_dump().await.expect("dump").entries;
    let entry = |name: &str| {
        dumped
            .iter()
            .find(|e| e.key == name)
            .unwrap_or_else(|| panic!("`{name}` is missing from the dump"))
            .clone()
    };

    let steps = entry(key::MAX_STEPS);
    assert_eq!(steps.value, serde_json::json!(5));
    assert_eq!(steps.layer, ConfigLayer::File, "it came from the document");

    let model = entry(key::MODEL);
    assert_eq!(
        model.value,
        serde_json::json!(EngineConfig::default().default_model)
    );
    assert_eq!(model.layer, ConfigLayer::Default);

    // A key the engine does not settle is still reported, as the document has
    // it: one list, not two that have to be reconciled.
    assert_eq!(entry("log.level").value, serde_json::json!("debug"));
    assert_eq!(entry("log.level").layer, ConfigLayer::File);
}

/// TC-BOOT-5: a document that cannot be read fails the boot, with the path.
/// The engine does not fall back to defaults behind the user's back.
#[test]
fn a_document_that_cannot_be_read_fails_the_boot() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    std::fs::write(&path, "sessions: [not, a, map]\n:\n  broken\n").expect("write");

    let error = boot::document(&path).expect_err("a document that does not parse");
    assert!(
        matches!(&error, ConfigError::Malformed { path: named, .. } if named == &path),
        "said: {error}"
    );
}
