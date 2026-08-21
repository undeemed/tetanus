//! Test Design Specification: per-namespace schemas, ported.
//!
//! Feature under test: `tetanus_config::schema` - what a namespace declares
//! about its own keys, and the three things that declaration decides: a scalar
//! written where a section belongs, a value of the wrong shape, and which
//! values must never be published. Upstream reaches the same three answers
//! through schemastery schemas installed per namespace
//! (`packages/settings/settings`), pinned by its `settings.spec.ts` and
//! `redact.spec.ts`.
//!
//! Approach: the schema against flat documents, directly. The wiring - the
//! engine's own declaration, the boot that checks a document against it, and
//! the dump that redacts by it - is asserted in `crates/engine/tests/boot.rs`
//! and `catalog.rs`, so a rule that is right and unwired fails there and not
//! here.
//!
//! What is not restated, and why. Upstream's schema vocabulary is a whole
//! validator library: ranges, patterns, enums, defaults, nested objects and
//! dictionaries. This declares a coarse kind, because the questions a *settings*
//! schema must answer that nothing else can are the three above; a range is a
//! check the reader that wants the value can make with the value in hand.
//! Upstream's `credential-ref` role, which points at where a credential lives
//! rather than holding one, has no counterpart: tetanus already spells that as
//! a separate key (`api_key_env`), and TC-SECRET-* pins that the two are told
//! apart by name.
//!
//! Environmental needs: none. No case touches a filesystem, a network or a
//! process-wide environment variable.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use serde_json::json;
use tetanus_config::schema::{Field, Kind, Schema};
use tetanus_config::{ConfigError, Document};

fn schema() -> Schema {
    Schema::new()
        .with("llm.model", Field::new(Kind::Text))
        .with("llm.retry.max_retries", Field::new(Kind::Integer))
        .with("llm.retry.jitter_ratio", Field::new(Kind::Number))
        .with("llm.retry.codes", Field::new(Kind::List))
        .with("log.verbose", Field::new(Kind::Boolean))
        .with(
            "llm.providers.deepseek.auth",
            Field::new(Kind::Text).secret(),
        )
        .with("deploy.token_count", Field::new(Kind::Integer))
}

fn document(pairs: [(&str, serde_json::Value); 1]) -> Document {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

/// TC-PORT-SET-5: a scalar written where a section belongs is refused.
///
/// Upstream refuses the write, because its namespace carries a schema and a
/// string is not an object. This case previously pinned the *other* answer -
/// that a flat model with no schema could only ignore it - and named the schema
/// as the open question behind it. The schema is now here, so the case asserts
/// what upstream asserts.
///
/// Input: `llm: off`, and `llm.retry: off`, over a schema declaring keys under
/// both.
/// Expected: refused, naming the key and saying to write the keys inside it.
/// Ignoring it is what made a user who thought they had turned something off
/// have changed nothing at all, with nothing saying so.
#[test]
fn a_scalar_written_where_a_section_belongs_is_refused() {
    let schema = schema();

    let shallow = schema.check(&document([("llm", json!("off"))]));
    let nested = schema.check(&document([("llm.retry", json!(false))]));

    assert!(
        matches!(&shallow[..], [ConfigError::SectionExpected { key, .. }] if key == "llm"),
        "{shallow:?}"
    );
    assert!(
        matches!(&nested[..], [ConfigError::SectionExpected { key, .. }] if key == "llm.retry"),
        "{nested:?}"
    );
    assert!(shallow[0].to_string().contains("Write the keys inside it"));
}

/// TC-PORT-SET-6: a key is only a section when a declared key sits under it.
///
/// Upstream's namespaces are objects; here a section is a prefix, so the rule
/// has to be exact about what a prefix is.
///
/// Input: `llm.model`, which is declared and holds no keys under it; and a key
/// whose name merely starts with a declared section's name.
/// Expected: both accepted. The dot is what makes a section, so `llm_mode` is
/// not made one by `llm.model` existing - a rule on the string alone would
/// refuse settings that have nothing to do with each other.
#[test]
fn a_prefix_is_only_a_section_when_a_declared_key_sits_under_it() {
    let schema = schema();

    assert!(schema.is_section("llm"));
    assert!(schema.is_section("llm.retry"));
    assert!(!schema.is_section("llm.model"), "a leaf is not a section");
    assert!(!schema.is_section("llm_mode"), "the dot is what makes one");
    assert!(schema
        .check(&document([("llm.model", json!("deepseek-chat"))]))
        .is_empty());
    assert!(schema
        .check(&document([("llm_mode", json!("off"))]))
        .is_empty());
}

/// TC-PORT-SET-7: a value of the wrong kind is refused, naming what the key
/// takes.
///
/// Upstream's schema refuses the same writes at the same point.
///
/// Input: text where a number belongs, a fraction where a whole number
/// belongs, and a scalar where a list belongs.
/// Expected: refused, each naming the key and the kind. A budget written as
/// `"eight"` used to be discovered mid-run by whichever reader got to it first,
/// if anything read it at all.
#[test]
fn a_value_of_the_wrong_kind_is_refused_and_says_what_the_key_takes() {
    let schema = schema();

    let text = schema.check(&document([("llm.retry.max_retries", json!("eight"))]));
    let fraction = schema.check(&document([("llm.retry.max_retries", json!(2.5))]));
    let scalar = schema.check(&document([("llm.retry.codes", json!("RATE_LIMIT"))]));

    assert!(
        matches!(&text[..], [ConfigError::BadValue { key, .. }] if key == "llm.retry.max_retries")
    );
    assert_eq!(fraction.len(), 1, "2.5 is not a whole number");
    assert!(
        matches!(&scalar[..], [ConfigError::BadValue { expected, .. }] if expected == "a list")
    );
    assert!(
        text[0].to_string().contains("a whole number"),
        "{}",
        text[0]
    );
}

/// TC-PORT-SET-8: a whole number written with a decimal point is a whole
/// number.
///
/// The same judgement `tetanus_turn::schema` makes about a model's arguments,
/// for the same reason.
///
/// Input: `2.0` where a whole number belongs, and a float where a number
/// belongs.
/// Expected: both accepted. A document that spells a whole number with a
/// decimal point has still said the number, and refusing it would fail a
/// deployment over a spelling.
#[test]
fn a_whole_number_with_a_decimal_point_is_still_whole() {
    let schema = schema();

    assert!(schema
        .check(&document([("llm.retry.max_retries", json!(2.0))]))
        .is_empty());
    assert!(schema
        .check(&document([("llm.retry.jitter_ratio", json!(0.25))]))
        .is_empty());
    assert_eq!(
        schema
            .check(&document([("llm.retry.jitter_ratio", json!("a quarter"))]))
            .len(),
        1
    );
}

/// TC-PORT-SET-9: a key no namespace declares is still accepted.
///
/// Upstream refuses an unknown key in a namespace it owns; tetanus does not,
/// deliberately.
///
/// Input: a key nothing declares, and a key under a declared section that the
/// section did not declare.
/// Expected: both accepted. A schema here narrows what can go wrong; making it
/// a whitelist would turn it into a second register every plugin must join
/// before its settings can be written, and the first casualty would be a
/// deployment configuring a tool this build does not ship.
#[test]
fn a_key_nothing_declares_is_accepted() {
    let schema = schema();

    assert!(schema
        .check(&document([("something.else", json!(1))]))
        .is_empty());
    assert!(schema
        .check(&document([("llm.retry.undeclared", json!(true))]))
        .is_empty());
}

/// TC-PORT-SET-10: redaction follows the declaration, and the name only where
/// nothing declared.
///
/// Upstream reads a `secret` role off the schema; tetanus had only the key's
/// last word, which decides wrongly in both directions.
///
/// Input: a declared secret whose name says nothing; a declared non-secret
/// whose name ends in a secret word; and an undeclared key whose name does.
/// Expected: hidden, published, hidden. The declaration is not a guess - it
/// catches `llm.providers.deepseek.auth`, which no name rule would, and stops
/// hiding `deploy.token_count`, which the name rule does.
#[test]
fn a_declaration_decides_redaction_and_the_name_is_the_fallback() {
    let schema = schema();

    assert!(
        schema.is_secret("llm.providers.deepseek.auth"),
        "a declared secret is hidden however it is spelled"
    );
    assert!(
        !schema.is_secret("deploy.token_count"),
        "a declared field that is not a secret is published, whatever its name ends in"
    );
    assert!(
        schema.is_secret("some.other.api_key"),
        "a key nothing declares still falls back to the name rule"
    );
    assert!(!schema.is_secret("llm.model"));
}

/// TC-PORT-SET-11: every fault in one document is reported, and `accept`
/// carries them together.
///
/// Upstream reports per namespace as each is installed; the reason is the same
/// either way.
///
/// Input: a document with three different faults.
/// Expected: `check` answers all three in key order; `accept` answers one error
/// whose message names the first and counts the rest. A user fixing a settings
/// file one message at a time needs one run of the harness per mistake.
#[test]
fn every_fault_in_one_document_is_reported() {
    let schema = schema();
    let document: Document = [
        ("llm".to_string(), json!("off")),
        ("llm.retry.max_retries".to_string(), json!("eight")),
        ("log.verbose".to_string(), json!("yes")),
    ]
    .into_iter()
    .collect();

    let faults = schema.check(&document);
    let refused = schema.accept(document).expect_err("refused");

    assert_eq!(faults.len(), 3, "{faults:?}");
    assert!(matches!(refused, ConfigError::Rejected { .. }));
    let message = refused.to_string();
    assert!(message.contains("2 more problems"), "{message}");
    assert!(message.contains("log.verbose"), "{message}");
}

/// TC-PORT-SET-12: a document with nothing wrong is handed back unchanged.
///
/// The path every ordinary boot takes.
///
/// Input: a document setting one declared key of each kind.
/// Expected: accepted, with the same keys and values. A checker that
/// normalized what it read would be a second thing deciding what a setting
/// means.
#[test]
fn a_document_with_nothing_wrong_is_handed_back_as_it_was() {
    let schema = schema();
    let document: Document = [
        ("llm.model".to_string(), json!("deepseek-chat")),
        ("llm.retry.max_retries".to_string(), json!(3)),
        ("llm.retry.codes".to_string(), json!(["RATE_LIMIT"])),
        ("log.verbose".to_string(), json!(true)),
    ]
    .into_iter()
    .collect();

    let accepted = schema.accept(document.clone()).expect("accepted");

    assert_eq!(accepted, document);
    assert!(!schema.is_empty());
    assert_eq!(schema.keys().count(), 7);
}
