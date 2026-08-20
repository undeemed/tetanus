//! Test Design Specification: what a settings document overrides, and what it
//! leaves alone.
//!
//! Feature under test: the two halves of `tetanus_config` that decide it
//! together - `file::read`, which flattens a nested document into dotted keys,
//! and `Config::load`, which resolves one key across the layers. Neither half
//! merges anything on its own. The merge a user sees is the pair.
//!
//! Approach: a real document in a temporary directory, loaded as
//! `Layer::File` over a `Layer::Default` that stands for the compiled
//! defaults, because the rule under test is what a user's file does to a value
//! they did not write. Each case names the upstream case it restates.
//!
//! Upstream reaches the same answers through a different mechanism: a
//! registered schema per namespace, and a `deepMerge` of the user section over
//! the base. tetanus has no schema and no merge step, so these cases exist to
//! pin that the flat model still answers the same way - and, in TC-PORT-SET-5,
//! where it does not.
//!
//! Features NOT tested here: reading and parsing the document
//! (`upstream_settings.rs`), layer precedence for one key (`layers.rs`), and
//! re-reading an edited document (`upstream_recompose.rs`).
//!
//! Environmental needs: a writable temp directory. No case reads a
//! process-wide environment variable, and none reaches a network.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;
use tetanus_config::{file, Config, Document, Layer};

/// TC-PORT-SET-1: a document that restates one leaf of a section leaves the
/// rest of that section to the layer below.
///
/// Upstream: `settings.spec.ts`, `deep-merges nested objects`.
///
/// Input: defaults setting three keys of `llm.retry`, and a document that
/// writes only `max_retries`.
/// Expected: `max_retries` is the document's, attributed to `File`; `mode` and
/// `retryable_codes` keep the default values, attributed to `Default`. This is
/// the promise that lets a user change one value without restating the section
/// around it, and tetanus keeps it without a merge step: the section is not a
/// value, so a document cannot replace one.
#[test]
fn a_document_that_writes_one_leaf_keeps_the_rest_of_the_section() {
    let dir = TempDir::new().expect("temp dir");
    let mut config = defaults();

    config.load(
        Layer::File,
        read(&document(&dir, "llm:\n  retry:\n    max_retries: 5\n")),
    );

    assert_eq!(
        resolved(&config, "llm.retry.max_retries"),
        (json!(5), Layer::File)
    );
    assert_eq!(
        resolved(&config, "llm.retry.mode"),
        (json!("normal"), Layer::Default)
    );
    assert_eq!(
        resolved(&config, "llm.retry.retryable_codes"),
        (json!(["SERVER", "THROTTLE"]), Layer::Default)
    );
}

/// TC-PORT-SET-2: a list is one value, so a document replaces it whole.
///
/// Upstream: `settings.spec.ts`, `replaces arrays wholesale`.
///
/// Input: a default `retryable_codes` of two entries, and a document writing a
/// list of one.
/// Expected: the resolved value is the document's list alone - not a union,
/// not the default with one entry overwritten - and the flattening mints no
/// per-element key. The second assertion is the one that would fail first if
/// the reader ever recursed into a list: a `retryable_codes.0` key would make
/// two layers' lists merge element by element, which is how a code a user
/// deleted comes back.
#[test]
fn a_list_a_document_writes_replaces_the_one_below_it() {
    let dir = TempDir::new().expect("temp dir");
    let mut config = defaults();

    config.load(
        Layer::File,
        read(&document(
            &dir,
            "llm:\n  retry:\n    retryable_codes: [SERVER]\n",
        )),
    );

    assert_eq!(
        resolved(&config, "llm.retry.retryable_codes"),
        (json!(["SERVER"]), Layer::File)
    );
    let indexed: Vec<&String> = config
        .provenance()
        .map(|(key, _)| key)
        .filter(|key| key.starts_with("llm.retry.retryable_codes."))
        .collect();
    assert!(
        indexed.is_empty(),
        "the list was flattened into {indexed:?}"
    );
}

/// TC-PORT-SET-3: a section written empty sets nothing.
///
/// Upstream: `settings.spec.ts`, `ignores explicit undefined entries so a
/// sparse patch cannot erase keys` - the same promise, that writing a section
/// cannot erase what it does not mention.
///
/// Input: a document whose `llm.retry` section is an empty map.
/// Expected: every default under it still resolves from `Default`, and the
/// document contributes no key at all - not even `llm.retry` itself. An empty
/// map that contributed a key would shadow nothing, but it would appear in
/// `config.dump` as a value the user set, which is a lie about the document.
#[test]
fn a_section_written_empty_sets_nothing() {
    let dir = TempDir::new().expect("temp dir");
    let read_back = read(&document(&dir, "llm:\n  retry: {}\n"));
    let mut config = defaults();

    config.load(Layer::File, read_back.clone());

    assert_eq!(read_back, Document::new());
    assert_eq!(
        resolved(&config, "llm.retry.mode"),
        (json!("normal"), Layer::Default)
    );
    assert!(
        config
            .provenance()
            .all(|(_, value)| value.layer == Layer::Default),
        "an empty section contributed a key"
    );
}

/// TC-PORT-SET-4: a key written with no value is a null the user set.
///
/// Upstream has no counterpart: JavaScript distinguishes `undefined` from
/// `null`, so its patch rule can ignore one and store the other. A document
/// has only what is written, and `mode:` on its own line is written.
///
/// Input: a document writing `mode:` with nothing after it.
/// Expected: `llm.retry.mode` resolves to JSON null, attributed to `File`, so
/// the default does not come back. The value is then a value like any other,
/// and the reader that wants a word out of that key refuses it by the ordinary
/// rule rather than silently running the default the user had struck out.
#[test]
fn a_key_written_with_no_value_is_a_null_not_an_absence() {
    let dir = TempDir::new().expect("temp dir");
    let mut config = defaults();

    config.load(
        Layer::File,
        read(&document(&dir, "llm:\n  retry:\n    mode:\n")),
    );

    assert_eq!(
        resolved(&config, "llm.retry.mode"),
        (json!(null), Layer::File)
    );
}

/// TC-PORT-SET-5: a scalar written where a section belongs shadows nothing.
///
/// This is the limitation the flat model implies, pinned so it is a known
/// answer rather than a surprise. Upstream would refuse the write: its
/// namespace carries a schema, and a string is not an object.
///
/// Input: a document writing `llm: off`, over defaults that set keys inside
/// `llm`.
/// Expected: the document contributes the key `llm`, which nothing reads, and
/// every key under `llm.` still resolves from `Default`. tetanus resolves per
/// key and has no schema to say that `llm` is a section, so it cannot refuse
/// the write; what it must not do is let the scalar half-apply. Refusing a key
/// no reader claims is a schema decision, and `docs/parity.md` carries it as
/// the open one it is.
#[test]
fn a_scalar_written_where_a_section_belongs_shadows_nothing() {
    let dir = TempDir::new().expect("temp dir");
    let mut config = defaults();

    config.load(Layer::File, read(&document(&dir, "llm: off\n")));

    assert_eq!(resolved(&config, "llm"), (json!("off"), Layer::File));
    assert_eq!(
        resolved(&config, "llm.retry.mode"),
        (json!("normal"), Layer::Default)
    );
    assert_eq!(
        resolved(&config, "llm.retry.max_retries"),
        (json!(2), Layer::Default)
    );
}

/// Three keys of one section on the lowest layer, standing for the compiled
/// defaults a build ships with.
fn defaults() -> Config {
    let mut config = Config::default();
    config.set("llm.retry.mode", json!("normal"), Layer::Default);
    config.set("llm.retry.max_retries", json!(2), Layer::Default);
    config.set(
        "llm.retry.retryable_codes",
        json!(["SERVER", "THROTTLE"]),
        Layer::Default,
    );
    config
}

fn document(dir: &TempDir, text: &str) -> PathBuf {
    let path = dir.path().join(file::DOCUMENT);
    std::fs::write(&path, text).expect("write the document");
    path
}

fn read(path: &Path) -> Document {
    file::read(path).expect("the document reads")
}

/// One resolved key as the pair every case asserts on: the value, and the
/// layer it came from. Reading them together is the point - a value that is
/// right for the wrong reason is a resolution defect waiting for the next
/// document.
fn resolved(config: &Config, key: &str) -> (serde_json::Value, Layer) {
    let resolved = config
        .get(key)
        .unwrap_or_else(|| panic!("no key `{key}` resolved"));
    (resolved.value.clone(), resolved.layer)
}
