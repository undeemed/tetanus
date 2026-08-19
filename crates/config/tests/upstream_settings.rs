//! Test Design Specification: the settings document and the layers it feeds.
//!
//! Feature under test: `tetanus_config::file::read`, `tetanus_config::home`,
//! and the layer resolution in `Config`. Upstream pins the same decisions in
//! `packages/settings/settings-file/tests/local.spec.ts` (boot and reads) and
//! `packages/util/home-paths`; each case names the upstream case it restates.
//!
//! Approach: a real document in a temporary directory, because the rules under
//! test are about a file on disk. Home resolution uses `home_from`, so no case
//! sets a process-wide environment variable.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;
use tetanus_config::{file, home, Config, ConfigError, Layer};

/// TC-PORT-CFG-1: an absent document reads as no settings.
///
/// Upstream: "resolves defaults over an absent file".
///
/// Expected: an empty document and no error, so the defaults below the file
/// layer are what resolves. A first run has nothing on disk and must still run.
#[test]
fn an_absent_document_reads_as_no_settings() {
    let dir = TempDir::new().unwrap();
    let path = file::document_path(dir.path());

    let document = file::read(&path).expect("an absent document is not a fault");

    assert!(document.is_empty());
    let mut config = Config::default();
    config.set("log.level", json!("info"), Layer::Default);
    config.load(Layer::File, document);
    assert_eq!(config.get("log.level").unwrap().layer, Layer::Default);
}

/// TC-PORT-CFG-2: a YAML document's sections read as dotted keys.
///
/// Upstream: "reads sections from an existing yaml document".
///
/// Input: two sections, one of them nested, plus a list and a number.
/// Expected: one key per leaf, named `section.key`, with the leaf's JSON type
/// preserved. A list is one value, not a branch.
#[test]
fn a_yaml_document_reads_as_dotted_keys() {
    let dir = TempDir::new().unwrap();
    let path = write(
        &dir,
        "settings.yaml",
        "log:\n  level: debug\n  sinks: [stderr]\nui:\n  theme:\n    name: dark\n    contrast: 3\n",
    );

    let document = file::read(&path).unwrap();

    assert_eq!(
        document.keys().collect::<Vec<_>>(),
        vec![
            "log.level",
            "log.sinks",
            "ui.theme.contrast",
            "ui.theme.name"
        ]
    );
    assert_eq!(document["log.sinks"], json!(["stderr"]));
    assert_eq!(document["ui.theme.contrast"], json!(3));
}

/// TC-PORT-CFG-3: a JSON document reads the same way.
///
/// Upstream: "reads sections from a json document".
///
/// Expected: the same keys and values a YAML document of the same shape gives.
/// The format is how the user wrote it down, not what the harness resolves.
#[test]
fn a_json_document_reads_the_same_keys() {
    let dir = TempDir::new().unwrap();
    let path = write(
        &dir,
        "settings.json",
        r#"{ "log": { "level": "debug", "sinks": ["stderr"] } }"#,
    );

    let document = file::read(&path).unwrap();

    assert_eq!(document["log.level"], json!("debug"));
    assert_eq!(document["log.sinks"], json!(["stderr"]));
}

/// TC-PORT-CFG-4: an empty document is no sections.
///
/// Upstream: "reads an empty yaml document as no sections" and its JSON twin.
///
/// Input: an emptied YAML file, a comment-only YAML file, and an emptied JSON
/// file.
/// Expected: an empty document from each. An editor that saves an emptied file
/// has said "nothing configured", which JSON cannot spell as a parsable value.
#[test]
fn an_empty_document_is_no_sections() {
    let dir = TempDir::new().unwrap();

    for (name, text) in [
        ("settings.yaml", ""),
        ("comments.yaml", "# nothing set yet\n"),
        ("settings.json", "   \n"),
    ] {
        let path = write(&dir, name, text);
        assert!(file::read(&path).unwrap().is_empty(), "{name}");
    }
}

/// TC-PORT-CFG-5: an unsupported extension fails loud.
///
/// Upstream: "fails loud on an unsupported extension".
///
/// Expected: `UnsupportedExtension` naming the extension, before any read. A
/// `settings.txt` the harness quietly ignored would look configured and behave
/// as if it were not.
#[test]
fn an_unsupported_extension_fails_loud() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "settings.txt", "log:\n  level: debug\n");

    let error = file::read(&path).expect_err("an unreadable format is a fault");

    assert!(
        matches!(&error, ConfigError::UnsupportedExtension { extension, .. } if extension == "txt"),
        "{error}"
    );
    assert!(error.to_string().contains(".json, .yaml or .yml"));
}

/// TC-PORT-CFG-6: a directory at the document path fails loud.
///
/// Upstream: "fails loud when the document path names a directory".
///
/// Expected: `IsADirectory`, not the platform's read error, so the message
/// reads the same wherever it is reported.
#[test]
fn a_directory_at_the_document_path_fails_loud() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.yaml");
    std::fs::create_dir(&path).unwrap();

    let error = file::read(&path).expect_err("a directory is not a document");

    assert!(matches!(error, ConfigError::IsADirectory { .. }), "{error}");
}

/// TC-PORT-CFG-7: a document that does not parse fails loud.
///
/// Upstream: "fails loud at boot on unparsable yaml".
///
/// Expected: `Malformed`, carrying the parser's own message so the user can
/// find the line.
#[test]
fn a_document_that_does_not_parse_fails_loud() {
    let dir = TempDir::new().unwrap();
    let path = write(
        &dir,
        "settings.yaml",
        "log:\n  level: debug\n :\n\t- broken\n",
    );

    let error = file::read(&path).expect_err("a torn document is a fault");

    assert!(matches!(error, ConfigError::Malformed { .. }), "{error}");
}

/// TC-PORT-CFG-8: a root that is not a map fails loud.
///
/// Upstream: "fails loud at boot when the root is not a map of sections".
///
/// Input: a document whose root is a list.
/// Expected: `NotAMap`. There is no key to resolve a bare list to, so honouring
/// it would mean dropping it.
#[test]
fn a_root_that_is_not_a_map_fails_loud() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "settings.yaml", "- log\n- ui\n");

    let error = file::read(&path).expect_err("a list of nothing configures nothing");

    assert!(matches!(error, ConfigError::NotAMap { .. }), "{error}");
}

/// TC-PORT-CFG-9: the document sits under the harness home.
///
/// Upstream: "defaults the file location under the configured harness home",
/// and `resolveDshHome`.
///
/// Expected, in precedence order: an explicit path wins; `$TETANUS_HOME` is
/// next; `~/.tetanus` is the default. A leading `~` expands, and a blank
/// override counts as unset - an empty `$TETANUS_HOME` must never put every
/// harness file in the working directory.
#[test]
fn the_document_sits_under_the_harness_home() {
    let configured = PathBuf::from("/srv/tetanus");
    assert_eq!(
        file::document_path(&home::home_from(Some(&configured), Some("/env/home"))),
        Path::new("/srv/tetanus/settings.yaml")
    );
    assert_eq!(
        home::home_from(None, Some("/env/home")),
        Path::new("/env/home")
    );

    let os_home = PathBuf::from(std::env::var_os("HOME").expect("a test host has a home"));
    assert_eq!(home::home_from(None, Some("   ")), os_home.join(".tetanus"));
    assert_eq!(home::home_from(None, None), os_home.join(".tetanus"));
    assert_eq!(
        home::home_from(None, Some("~/nested")),
        os_home.join("nested")
    );
}

/// TC-PORT-CFG-10: a reloaded layer drops the keys it stopped setting.
///
/// Upstream: the settings-file provider republishes the whole document on an
/// external edit, and a removed section resolves through its defaults again
/// (`watcher.spec.ts`, "folds an unobserved external edit into a write").
///
/// Input: a default, a file layer that overrides it and sets a second key,
/// then a second file layer holding only the second key.
/// Expected: while the file sets it, `log.level` resolves from `File`; once the
/// re-read document drops it, the default is back, and the key the file still
/// sets keeps its new value. Provenance names the winning layer throughout.
#[test]
fn a_reloaded_layer_drops_the_keys_it_stopped_setting() {
    let mut config = Config::default();
    config.set("log.level", json!("info"), Layer::Default);

    config.load(
        Layer::File,
        [
            ("log.level".to_string(), json!("debug")),
            ("ui.theme".to_string(), json!("dark")),
        ]
        .into(),
    );
    assert_eq!(config.get("log.level").unwrap().value, json!("debug"));
    assert_eq!(config.get("log.level").unwrap().layer, Layer::File);

    config.load(
        Layer::File,
        [("ui.theme".to_string(), json!("light"))].into(),
    );
    let level = config.get("log.level").unwrap();
    assert_eq!(
        (level.value.clone(), level.layer),
        (json!("info"), Layer::Default)
    );
    assert_eq!(config.get("ui.theme").unwrap().value, json!("light"));
}

fn write(dir: &TempDir, name: &str, text: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path
}
