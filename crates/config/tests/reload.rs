//! Test Design Specification: the watcher driving the re-read, ported.
//!
//! Feature under test: `tetanus_config::reload` - the piece that turns "the
//! document settled into a new state" into "the running configuration now says
//! this". Upstream's watcher republishes resolved settings on every settled
//! edit (`packages/settings/settings-file`, `watcher.spec.ts`); the two halves
//! existed here and nothing joined them, so a user who edited `settings.yaml`
//! while the harness ran still saw nothing happen.
//!
//! Approach: real files for the cases about what is on disk, and driven
//! observations for the cases about the settling rule - a case that wrote a
//! file twice quickly would be testing a filesystem's timestamp granularity
//! rather than the decision.
//!
//! What is not restated, and why. Upstream's dispose-quiesce (a watcher that
//! stops cleanly while a callback is in flight) has no counterpart: this is a
//! step a caller takes rather than a thread with callbacks, so there is nothing
//! in flight when the caller stops calling. The debounce itself is
//! `tetanus_config::watch`'s and is pinned by TC-WATCH-*.
//!
//! Environmental needs: a writable temporary directory.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;
use tetanus_config::reload::{load, Change, Reload};
use tetanus_config::schema::{Field, Kind, Schema};
use tetanus_config::{Config, Document, Layer};

fn schema() -> Schema {
    Schema::new()
        .with("llm.model", Field::new(Kind::Text))
        .with("llm.retry.max_retries", Field::new(Kind::Integer))
}

/// Defaults under the document, so a case can see a key fall back.
fn config() -> Config {
    let mut config = Config::default();
    config.load(
        Layer::Default,
        Document::from([
            ("llm.model".to_string(), json!("compiled-default")),
            ("llm.retry.max_retries".to_string(), json!(2)),
        ]),
    );
    config
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write the document");
}

fn value(config: &Config, key: &str) -> serde_json::Value {
    config.get(key).expect("resolved").value.clone()
}

/// TC-PORT-RELOAD-1: an edit that settles is read into the running
/// configuration.
///
/// Upstream republishes the resolved settings on every settled edit.
///
/// Input: a configuration on its defaults, then a document written and the
/// reload ticked.
/// Expected: the tick reports what changed, and a reader now resolves the
/// document's value from the file layer. This is the whole point of the block:
/// before it, the same edit changed nothing until a restart.
#[test]
fn an_edit_that_settles_reaches_the_running_configuration() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    let mut config = config();
    let mut reload = Reload::new(&path, schema());

    write(&path, "llm:\n  model: from-the-document\n");
    let change = reload.tick(&mut config);

    assert!(change.is_effective(), "{change:?}");
    assert!(
        matches!(&change, Change::Applied(recomposed) if recomposed.changed == ["llm.model"]),
        "{change:?}"
    );
    assert_eq!(value(&config, "llm.model"), json!("from-the-document"));
    assert_eq!(
        config.get("llm.model").expect("resolved").layer,
        Layer::File
    );
}

/// TC-PORT-RELOAD-2: a tick with nothing new does nothing.
///
/// Upstream's watcher fires per change, not per poll.
///
/// Input: two ticks with no edit between them, then a save that rewrites the
/// same bytes.
/// Expected: `None` every time, and no work done. A caller that republishes on
/// every tick would republish nothing new all day, and an editor saving an
/// unmodified buffer is the common case.
#[test]
fn a_tick_with_nothing_new_does_nothing() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    write(&path, "llm:\n  model: settled\n");
    let mut config = config();
    let mut reload = Reload::new(&path, schema());

    let first = reload.tick(&mut config);
    let second = reload.tick(&mut config);

    assert!(matches!(first, Change::None), "{first:?}");
    assert!(matches!(second, Change::None), "{second:?}");
    assert_eq!(
        value(&config, "llm.model"),
        json!("compiled-default"),
        "the baseline is what is there, so starting is not an edit"
    );
}

/// TC-PORT-RELOAD-3: a key the document drops falls back to the layer below.
///
/// Upstream's re-read is a replacement of the layer, not a merge into it.
///
/// Input: a document that sets a key, then one that no longer does.
/// Expected: the key resolves from `Default` again, and the change is reported.
/// This is why the layers are kept separately: a folded map would have nothing
/// to fall back to.
#[test]
fn a_key_the_document_drops_falls_back_to_the_layer_below() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    let mut config = config();
    let mut reload = Reload::new(&path, schema());
    write(&path, "llm:\n  model: from-the-document\n");
    reload.tick(&mut config);

    write(&path, "llm:\n  retry:\n    max_retries: 9\n");
    let change = reload.tick(&mut config);

    assert!(change.is_effective(), "{change:?}");
    assert_eq!(value(&config, "llm.model"), json!("compiled-default"));
    assert_eq!(
        config.get("llm.model").expect("resolved").layer,
        Layer::Default
    );
    assert_eq!(value(&config, "llm.retry.max_retries"), json!(9));
}

/// TC-PORT-RELOAD-4: a bad edit is reported, changes nothing, and the watching
/// goes on.
///
/// Upstream keeps watching after a failed read for the same reason.
///
/// Input: a good document, then broken YAML, then a good document again.
/// Expected: refused in the middle with the running configuration untouched,
/// and the third edit applied. A watcher that stopped at the typo would make it
/// permanent until a restart, and the user's next action - fixing it - would
/// appear to do nothing.
#[test]
fn a_bad_edit_is_reported_and_the_watching_goes_on() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    let mut config = config();
    let mut reload = Reload::new(&path, schema());
    write(&path, "llm:\n  model: good\n");
    reload.tick(&mut config);

    write(&path, "llm:\n  model: [unclosed\n");
    let refused = reload.tick(&mut config);
    let during = value(&config, "llm.model");
    write(&path, "llm:\n  model: better\n");
    let recovered = reload.tick(&mut config);

    assert!(matches!(refused, Change::Refused(_)), "{refused:?}");
    assert_eq!(during, json!("good"), "the running configuration stands");
    assert!(recovered.is_effective(), "{recovered:?}");
    assert_eq!(value(&config, "llm.model"), json!("better"));
}

/// TC-PORT-RELOAD-5: the schema is applied at run time, not only at boot.
///
/// The rule `crates/config/src/schema.rs` states, extended to the re-read.
///
/// Input: a running configuration, then a document edited to put a scalar where
/// a section belongs.
/// Expected: refused, naming the key, with every key under the section still
/// resolving as it did. A harness that booted clean must not be editable into a
/// state it would have refused to start in.
#[test]
fn a_document_the_schema_refuses_is_refused_at_run_time_too() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    let mut config = config();
    let mut reload = Reload::new(&path, schema());
    write(&path, "llm:\n  model: good\n");
    reload.tick(&mut config);

    write(&path, "llm: off\n");
    let refused = reload.tick(&mut config);

    assert!(
        matches!(&refused, Change::Refused(error) if error.to_string().starts_with("llm:")),
        "{refused:?}"
    );
    assert_eq!(value(&config, "llm.model"), json!("good"));
}

/// TC-PORT-RELOAD-6: a deleted document hands every key it set back.
///
/// Upstream treats a deletion as an edit like any other.
///
/// Input: a document that sets a key, then removed from disk.
/// Expected: the key resolves from the default again and the change is
/// reported. Absence is a state, not an error: deleting the file is how a user
/// says "go back to the defaults".
#[test]
fn a_deleted_document_hands_every_key_back() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    let mut config = config();
    let mut reload = Reload::new(&path, schema());
    write(&path, "llm:\n  model: from-the-document\n");
    reload.tick(&mut config);

    std::fs::remove_file(&path).expect("remove");
    let change = reload.tick(&mut config);

    assert!(change.is_effective(), "{change:?}");
    assert_eq!(value(&config, "llm.model"), json!("compiled-default"));
}

/// TC-PORT-RELOAD-7: a change is only applied once it has stopped moving.
///
/// Upstream's `stabilityThreshold`, restated over driven observations.
///
/// Input: a reload settling after two identical polls, driven through a
/// sequence: one new state, the same state again, then a third state.
/// Expected: nothing on the first sighting, applied on the repeat, nothing
/// again on the moving one. An editor that truncates and rewrites is
/// momentarily an empty document, and a reader that fired on the first event
/// would hand a running harness the defaults.
#[test]
fn a_change_is_applied_only_once_it_has_stopped_moving() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    write(&path, "llm:\n  model: settled\n");
    let mut config = config();
    let mut reload = Reload::new(&path, schema()).settle_after(2);
    // The states are driven rather than written, so the case is about the rule
    // and not about how fast this machine's clock ticks.
    let seen = |len: u64| tetanus_config::watch::Stamp {
        present: true,
        len,
        modified: None,
    };

    let first = reload.observe(seen(10), &mut config);
    let held = reload.observe(seen(10), &mut config);
    let moving = reload.observe(seen(20), &mut config);

    assert!(matches!(first, Change::None), "{first:?}");
    assert!(
        matches!(held, Change::Applied(_)),
        "the same state twice is settled: {held:?}"
    );
    assert!(matches!(moving, Change::None), "{moving:?}");
}

/// TC-PORT-RELOAD-8: the startup read is a separate call, and it checks the
/// schema too.
///
/// The half a `Reload` deliberately does not do.
///
/// Input: `load` against a good document, then against one the schema refuses.
/// Expected: the first fills the file layer; the second is refused and leaves
/// the configuration exactly as it was. Conflating this with the watcher would
/// make every boot look like an edit; leaving the check out of it would let a
/// document that cannot be re-read at run time still start a harness.
#[test]
fn the_startup_read_is_separate_and_checks_the_schema() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    let mut config = config();
    write(&path, "llm:\n  model: from-the-document\n");

    load(&mut config, &path, &schema()).expect("loaded");
    let after_good = value(&config, "llm.model");
    write(&path, "llm:\n  retry: off\n");
    let refused = load(&mut config, &path, &schema()).expect_err("refused");

    assert_eq!(after_good, json!("from-the-document"));
    assert!(refused.to_string().starts_with("llm.retry:"), "{refused}");
    assert_eq!(
        value(&config, "llm.model"),
        json!("from-the-document"),
        "a refused load changes nothing"
    );
}
