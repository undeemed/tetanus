//! Test Design Specification: layer resolution and provenance.
//!
//! Feature under test: `tetanus_config::Config`, which holds one document per
//! layer and resolves `default < file < env < flag` across them.
//!
//! Approach: drive the layers directly. What a layer's document came from - a
//! settings file, the environment, a flag - is a separate concern with its own
//! cases; these fix what resolution does once the documents exist.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use serde_json::json;
use tetanus_config::{Config, Layer};

/// TC-CFG-1: the highest layer that sets a key wins, whatever order the layers
/// were written in.
///
/// Input: a flag, then a default, then a file, all setting `log.level`, written
/// lowest-precedence-last on purpose.
/// Expected: the flag's value, attributed to `Flag`. Resolution reads the
/// layers, so a late write to a low layer cannot displace a high one - which is
/// what lets boot fill defaults after a flag has already been parsed.
#[test]
fn the_highest_layer_that_sets_a_key_wins() {
    let mut config = Config::default();
    config.set("log.level", json!("trace"), Layer::Flag);
    config.set("log.level", json!("info"), Layer::Default);
    config.set("log.level", json!("debug"), Layer::File);

    let resolved = config.get("log.level").expect("three layers set it");

    assert_eq!(resolved.value, json!("trace"));
    assert_eq!(resolved.layer, Layer::Flag);
}

/// TC-CFG-2: provenance reports each key once, with the layer that won it.
///
/// Expected: one entry per key, in key order, each naming its winning layer.
/// This is what `config.dump` renders, so a configuration surface shows the
/// user which layer to edit to change a value.
#[test]
fn provenance_reports_each_key_once_with_its_winning_layer() {
    let mut config = Config::default();
    config.set("log.level", json!("info"), Layer::Default);
    config.set("log.level", json!("debug"), Layer::Env);
    config.set("ui.theme", json!("dark"), Layer::Default);

    let entries: Vec<(&str, Layer)> = config
        .provenance()
        .map(|(key, resolved)| (key.as_str(), resolved.layer))
        .collect();

    assert_eq!(
        entries,
        vec![("log.level", Layer::Env), ("ui.theme", Layer::Default)]
    );
}

/// TC-CFG-3: a reloaded layer drops the keys it stopped setting.
///
/// Upstream republishes the whole settings document on an external edit, and a
/// section the edit removed resolves through its defaults again
/// (`packages/settings/settings-file/tests/watcher.spec.ts`).
///
/// Input: a default, a file layer that overrides it and sets a second key, then
/// a second file document holding only the second key.
/// Expected: while the file sets it, `log.level` resolves from `File`; once the
/// re-read document drops it, the default is back, and the key the file still
/// sets keeps its new value. A layer that merged instead of replacing would
/// pin the removed value forever.
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

    let level = config.get("log.level").expect("the default is still set");
    assert_eq!(
        (level.value.clone(), level.layer),
        (json!("info"), Layer::Default)
    );
    assert_eq!(config.get("ui.theme").unwrap().value, json!("light"));
}

/// TC-CFG-4: a key no layer sets resolves to nothing.
///
/// Input: a key set only on the file layer, then a file document without it and
/// nothing below.
/// Expected: `get` reports nothing and provenance does not list it. An entry
/// left behind would render in `config.dump` as configured when it is not.
#[test]
fn a_key_no_layer_sets_resolves_to_nothing() {
    let mut config = Config::default();
    config.load(
        Layer::File,
        [("log.level".to_string(), json!("debug"))].into(),
    );

    config.load(Layer::File, Default::default());

    assert!(config.get("log.level").is_none());
    assert_eq!(config.provenance().count(), 0);
}
