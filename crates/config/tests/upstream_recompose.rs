//! Test Design Specification: re-reading the settings document at run time.
//!
//! Features under test: `tetanus_config::recompose`, the fold that replaces
//! the file layer with what the document holds now. Upstream pins the same
//! decisions in the runtime half of
//! `packages/settings/settings-file/tests/watcher.spec.ts`; each case names
//! the upstream case it comes from.
//!
//! Approach: a document in a temporary directory, edited between reads, over a
//! configuration that also carries a default and a flag layer. Upstream's
//! watcher itself - its debounce, its dispose quiesce, its write path, and the
//! recovery of a watcher that errored - has no surface in tetanus, so those
//! cases have nothing to restate and stay rows in `docs/parity.md`.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;
use tetanus_config::recompose::recompose;
use tetanus_config::{Config, ConfigError, Layer};

/// TC-PORT-CFG-11: an edit made while the harness runs is folded in, and the
/// keys it moved are named.
///
/// Upstream: "boots from cordis.yml and hot-publishes an external settings
/// edit" (`loader-composition.spec.ts`).
///
/// Input: a document read at boot, then rewritten with one value changed and
/// one key added.
/// Expected: both keys read the new values, attributed to the file layer, and
/// both are reported changed. The untouched key is not reported.
#[test]
fn an_edit_at_run_time_is_folded_in_and_named() {
    let (_dir, path) = document("log:\n  level: info\n  format: text\n");
    let mut config = booted(&path);

    write(
        &path,
        "log:\n  level: debug\n  format: text\nui:\n  theme: dark\n",
    );
    let changed = recompose(&mut config, &path).expect("recompose");

    assert_eq!(changed.changed, vec!["log.level", "ui.theme"]);
    assert_eq!(config.get("log.level").unwrap().value, json!("debug"));
    assert_eq!(config.get("log.level").unwrap().layer, Layer::File);
    assert_eq!(config.get("ui.theme").unwrap().value, json!("dark"));
}

/// TC-PORT-CFG-12: a key the document no longer sets falls back to the layer
/// under it.
///
/// Upstream: the settings service resolves "schema defaults, then composition
/// base, then the user layer" (`settings.spec.ts`), so removing the user's
/// entry restores the layer below rather than leaving the old value behind.
///
/// Input: a document that sets `log.level`, re-read after that key is deleted
/// from it, over a default layer that also sets it.
/// Expected: `log.level` reads the default's value, attributed to `Default`,
/// and is reported changed.
#[test]
fn a_dropped_key_falls_back_to_the_layer_under_it() {
    let (_dir, path) = document("log:\n  level: debug\n");
    let mut config = booted(&path);
    assert_eq!(config.get("log.level").unwrap().layer, Layer::File);

    write(&path, "ui:\n  theme: dark\n");
    let changed = recompose(&mut config, &path).expect("recompose");

    assert_eq!(changed.changed, vec!["log.level", "ui.theme"]);
    let level = config.get("log.level").expect("the default is still there");
    assert_eq!(level.value, json!("warn"));
    assert_eq!(level.layer, Layer::Default);
}

/// TC-PORT-CFG-13: a document that turns bad at run time leaves the running
/// configuration exactly as it was.
///
/// Upstream: "keeps the last good document when the file turns unreadable at
/// runtime".
///
/// Input: a good document, then the same path holding text that does not
/// parse.
/// Expected: the fault is returned naming the path, and every key still reads
/// what it read before. A bad edit must not empty a harness that is working.
#[test]
fn a_bad_document_leaves_the_last_good_configuration_standing() {
    let (_dir, path) = document("log:\n  level: debug\n");
    let mut config = booted(&path);

    write(&path, "log:\n  level: [unclosed\n");
    let error = recompose(&mut config, &path).expect_err("the document does not parse");

    assert!(matches!(error, ConfigError::Malformed { .. }), "{error}");
    assert!(error.to_string().contains(&path.display().to_string()));
    assert_eq!(config.get("log.level").unwrap().value, json!("debug"));
    assert_eq!(config.get("log.level").unwrap().layer, Layer::File);
}

/// TC-PORT-CFG-14: a document that is gone sets nothing, rather than being a
/// fault.
///
/// Upstream: "treats an event for a still-absent file as a no-op" - the
/// resolved value stays what the layers under the file say.
///
/// Input: a document read at boot, then deleted.
/// Expected: the recompose succeeds, `log.level` falls back to the default,
/// and the flag layer's key is untouched.
#[test]
fn a_deleted_document_sets_nothing_and_is_not_a_fault() {
    let (_dir, path) = document("log:\n  level: debug\n");
    let mut config = booted(&path);

    std::fs::remove_file(&path).expect("remove");
    let changed = recompose(&mut config, &path).expect("an absent document is no settings");

    assert_eq!(changed.changed, vec!["log.level"]);
    assert_eq!(config.get("log.level").unwrap().layer, Layer::Default);
    assert_eq!(config.get("model").unwrap().layer, Layer::Flag);
}

/// TC-PORT-CFG-15: a re-read that changes nothing reports nothing.
///
/// Upstream: the watcher republishes only what moved; an editor that saves an
/// unchanged file must not look like a configuration change.
///
/// Expected: the second recompose of an unedited document reports no keys, and
/// `is_empty()` agrees.
#[test]
fn an_unedited_document_reports_no_change() {
    let (_dir, path) = document("log:\n  level: debug\n");
    let mut config = booted(&path);

    let changed = recompose(&mut config, &path).expect("recompose");

    assert!(changed.is_empty(), "{changed:?}");
    assert_eq!(changed.changed, Vec::<String>::new());
}

/// TC-PORT-CFG-16: a re-read cannot displace a higher layer.
///
/// Upstream: "persists the merged user section without baking in the base
/// layer" - a layer's re-read is that layer's business, and what a read
/// returns is still resolved across all of them.
///
/// Input: a document that starts setting `model`, which a flag already sets.
/// Expected: `model` still reads the flag's value from the flag layer, and the
/// key is not reported changed, because nothing resolved differently.
#[test]
fn a_re_read_cannot_displace_a_higher_layer() {
    let (_dir, path) = document("log:\n  level: debug\n");
    let mut config = booted(&path);

    write(&path, "log:\n  level: debug\nmodel: from-the-file\n");
    let changed = recompose(&mut config, &path).expect("recompose");

    assert_eq!(changed.changed, Vec::<String>::new());
    assert_eq!(config.get("model").unwrap().value, json!("from-a-flag"));
    assert_eq!(config.get("model").unwrap().layer, Layer::Flag);
}

/// A settings document in a temporary directory.
fn document(text: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    write(&path, text);
    (dir, path)
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write");
}

/// A configuration as boot leaves it: defaults, the document, and one flag.
fn booted(path: &Path) -> Config {
    let mut config = Config::default();
    config.set("log.level", json!("warn"), Layer::Default);
    config.set("model", json!("from-a-flag"), Layer::Flag);
    config.load(Layer::File, tetanus_config::file::read(path).expect("read"));
    config
}
