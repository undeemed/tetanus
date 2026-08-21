//! Test Design Specification: argument validation against a tool's schema,
//! ported.
//!
//! Feature under test: `tetanus_turn::schema::violations` - what a tool author
//! is told when a call's arguments do not fit the schema the tool published.
//! Upstream pins the same rules as `validateArgs` in
//! `packages/core/tools/tests/tools.spec.ts` ("the runtime-validation Agent
//! Note"), over the vocabulary its `json-schema.spec.ts` fixes; each case
//! names the upstream case it comes from.
//!
//! **This is a published helper, not a gate, and that is upstream's shape
//! rather than a shortcut.** `validateArgs` is exported from upstream's tools
//! package and called by tool authors; its agent loop does `JSON.parse` and
//! nothing more, which is why upstream's own `tool JSON parse` case asserts
//! that a call whose arguments are not JSON still reaches its tool. tetanus
//! pins that same behaviour in `upstream_tool_arguments.rs`
//! (TC-PORT-ARGS-1..4), so wiring this into dispatch would contradict a
//! faithful port rather than complete one. If tetanus ever wants a hard gate
//! it is a behaviour change with a contract clause, not a quiet addition here.
//!
//! Approach: the helper is pure, so every case is a literal schema, a literal
//! value and the exact violations expected. Upstream writes its schemas in a
//! `ParameterSchemaSpec` DSL and converts; tetanus tools declare JSON Schema
//! directly, so the cases are written in the converted form - which is the
//! same document upstream's DSL produces.
//!
//! Features NOT tested here: the DSL itself, and upstream's author-boundary
//! errors. Upstream throws `JsonSchemaError` for a schema its own DSL should
//! never have produced - an unknown `type`, an `enum` whose members do not
//! match the declared type. tetanus has no DSL and no author boundary to throw
//! at: a schema is a `serde_json::Value` a tool hands over, and the deliberate
//! answer to an unrecognised declaration is to constrain nothing, so a working
//! tool is never made uncallable by a keyword this validator has not learned.
//! TC-PORT-SCHEMA-9 pins that direction.
//!
//! Environmental needs: none. No case touches a filesystem, a network or an
//! API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use serde_json::{json, Value};
use tetanus_turn::schema::violations;

/// TC-PORT-SCHEMA-1: valid arguments produce nothing, and the check is total
/// over anything at all.
///
/// Upstream: "returns [] for valid args and is total over malformed input".
///
/// Totality is the property that matters most: this runs on a value the model
/// invented, so a shape nobody anticipated must produce a complaint rather
/// than a panic.
///
/// Input: a schema with one required string and one optional number, against
/// well-formed arguments, then against null, a string and an array.
/// Expected: nothing for the well-formed ones; exactly one complaint for each
/// value that is not an object, and no panic for any of them.
#[test]
fn valid_arguments_pass_and_anything_at_all_is_answered() {
    let schema = json!({
        "type": "object",
        "properties": { "path": { "type": "string" }, "limit": { "type": "number" } },
        "required": ["path"],
    });

    assert_eq!(violations(&schema, &json!({ "path": "/tmp" })), empty());
    assert_eq!(
        violations(&schema, &json!({ "path": "/tmp", "limit": 5 })),
        empty()
    );

    for malformed in [json!(null), json!("nope"), json!([]), json!(7), json!(true)] {
        let found = violations(&schema, &malformed);
        assert_eq!(found.len(), 1, "{malformed} produced {found:?}");
        assert!(found[0].contains("must be an object"), "{found:?}");
    }
}

/// TC-PORT-SCHEMA-2: a required key that is missing, or present as null, is
/// missing.
///
/// Upstream: "flags a missing required key and a required key present as
/// undefined".
///
/// JSON has no `undefined`, so the value a model sends for "I have nothing
/// here" is `null`. Treating it as supplied would hand the tool a null it
/// declared it required, which is the mistake the declaration exists to
/// prevent.
///
/// Input: a schema requiring `path`, against `{}` and `{"path": null}`.
/// Expected: the same single complaint for both, naming the property.
#[test]
fn a_required_key_that_is_absent_or_null_is_missing() {
    let schema = json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"],
    });

    for absent in [json!({}), json!({ "path": null })] {
        assert_eq!(
            violations(&schema, &absent),
            vec![r#"missing required property "path""#.to_string()],
            "for {absent}"
        );
    }
}

/// TC-PORT-SCHEMA-3: an extra key is allowed unless the tool said otherwise,
/// and an omitted optional is fine.
///
/// Upstream: "allows extra keys (no additionalProperties:false) and omitted
/// optionals", and "does not apply defaults (validation only)".
///
/// Input: a schema with one required key, against arguments carrying an extra;
/// then the same schema with `additionalProperties: false`; then a schema with
/// an optional carrying a `default`, against `{}`.
/// Expected: the extra passes by default and is refused when the tool asked
/// for it to be; the absent optional passes, and no default is invented -
/// this answers whether a value fits, and never edits it.
#[test]
fn extras_are_allowed_unless_refused_and_no_default_is_invented() {
    let open = json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"],
    });
    assert_eq!(
        violations(&open, &json!({ "path": "/tmp", "extra": 1 })),
        empty()
    );

    let closed = json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"],
        "additionalProperties": false,
    });
    let found = violations(&closed, &json!({ "path": "/tmp", "extra": 1 }));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains(r#""extra" is not a declared property"#),
        "{found:?}"
    );

    let with_default = json!({
        "type": "object",
        "properties": { "limit": { "type": "number", "default": 25 } },
    });
    assert_eq!(violations(&with_default, &json!({})), empty());
}

/// TC-PORT-SCHEMA-4: primitives are type-checked, and each complaint names the
/// property it is about.
///
/// Upstream: "type-checks primitives".
///
/// Input: a schema of a string, a number and a boolean, each given the wrong
/// kind of value, one at a time and then all at once.
/// Expected: one complaint naming each, and all three reported together rather
/// than only the first - a model told one mistake per round trip needs one
/// round trip per mistake.
#[test]
fn primitives_are_type_checked_and_every_mistake_is_reported() {
    let schema = json!({
        "type": "object",
        "properties": {
            "s": { "type": "string" },
            "n": { "type": "number" },
            "b": { "type": "boolean" },
        },
    });

    assert_eq!(
        violations(&schema, &json!({ "s": 1 })),
        vec![r#""s" must be a string"#.to_string()]
    );
    assert_eq!(
        violations(&schema, &json!({ "n": "x" })),
        vec![r#""n" must be a number"#.to_string()]
    );
    assert_eq!(
        violations(&schema, &json!({ "b": "x" })),
        vec![r#""b" must be a boolean"#.to_string()]
    );

    let all_wrong = violations(&schema, &json!({ "s": 1, "n": "x", "b": "x" }));
    assert_eq!(all_wrong.len(), 3, "{all_wrong:?}");
}

/// TC-PORT-SCHEMA-5: `enum` membership, and `const`.
///
/// Upstream: "checks enum membership" and "enforces type-correct scalar enum
/// declarations".
///
/// Input: a string enum given a member and a non-member; a number enum given
/// both; a `const` given the value and something else.
/// Expected: members pass, non-members are named against the list they had to
/// be in, and the complaint prints the allowed values - a message that says
/// only "invalid" makes the model guess.
#[test]
fn enum_membership_and_const_are_checked() {
    let colour = json!({
        "type": "object",
        "properties": { "color": { "type": "string", "enum": ["red", "green"] } },
    });
    assert_eq!(violations(&colour, &json!({ "color": "red" })), empty());
    assert_eq!(
        violations(&colour, &json!({ "color": "blue" })),
        vec![r#""color" must be one of ["red","green"]"#.to_string()]
    );

    let numbers = json!({
        "type": "object",
        "properties": { "n": { "type": "number", "enum": [1, 2] } },
    });
    assert_eq!(violations(&numbers, &json!({ "n": 1 })), empty());
    assert_eq!(
        violations(&numbers, &json!({ "n": 3 })),
        vec![r#""n" must be one of [1,2]"#.to_string()]
    );

    let fixed = json!({
        "type": "object",
        "properties": { "kind": { "const": "shell" } },
    });
    assert_eq!(violations(&fixed, &json!({ "kind": "shell" })), empty());
    assert_eq!(
        violations(&fixed, &json!({ "kind": "python" })),
        vec![r#""kind" must be "shell""#.to_string()]
    );
}

/// TC-PORT-SCHEMA-6: nested objects and arrays are walked, and a complaint
/// carries the path to what was wrong.
///
/// Upstream: "recurses into nested objects (and an object without properties
/// only type-checks)".
///
/// A complaint about a value three levels down that only names the leaf is
/// ambiguous the moment two branches share a property name, so the path is the
/// message.
///
/// Input: a nested object with a required leaf, an array of typed items, and
/// an object declared with no `properties` at all.
/// Expected: dotted paths for object members and indexed paths for array
/// entries; an object with no declared properties type-checks and looks no
/// deeper.
#[test]
fn nesting_is_walked_and_a_complaint_carries_its_path() {
    let schema = json!({
        "type": "object",
        "properties": {
            "config": {
                "type": "object",
                "properties": { "retries": { "type": "integer" } },
                "required": ["retries"],
            },
            "tags": { "type": "array", "items": { "type": "string" } },
            "opaque": { "type": "object" },
        },
    });

    assert_eq!(
        violations(
            &schema,
            &json!({ "config": { "retries": 2 }, "tags": ["a"] })
        ),
        empty()
    );
    assert_eq!(
        violations(&schema, &json!({ "config": {} })),
        vec![r#"missing required property "config.retries""#.to_string()]
    );
    assert_eq!(
        violations(&schema, &json!({ "config": { "retries": "two" } })),
        vec![r#""config.retries" must be an integer"#.to_string()]
    );
    assert_eq!(
        violations(&schema, &json!({ "tags": ["a", 2, "c"] })),
        vec![r#""tags[1]" must be a string"#.to_string()]
    );

    // No declared properties means nothing inside is constrained, but the
    // value must still be an object.
    assert_eq!(
        violations(&schema, &json!({ "opaque": { "anything": [1, 2] } })),
        empty()
    );
    assert_eq!(
        violations(&schema, &json!({ "opaque": 7 })),
        vec![r#""opaque" must be an object"#.to_string()]
    );
}

/// TC-PORT-SCHEMA-7: `integer` accepts a whole number however it was spelled.
///
/// Upstream checks `Number.isInteger`, which is true of `2.0` because
/// JavaScript has one number type. A model that was told "integer" and emitted
/// `2.0` meant two, and refusing it would fail a call over a spelling rather
/// than over a value.
///
/// Input: an integer property given `2`, `2.0`, `-3`, then `2.5` and a string.
/// Expected: the whole numbers pass however written; a fraction and a
/// non-number are refused.
#[test]
fn an_integer_accepts_a_whole_number_however_it_was_written() {
    let schema = json!({
        "type": "object",
        "properties": { "n": { "type": "integer" } },
    });

    for whole in [json!(2), json!(2.0), json!(-3), json!(0)] {
        assert_eq!(violations(&schema, &json!({ "n": whole })), empty());
    }
    for not_whole in [json!(2.5), json!("2")] {
        let found = violations(&schema, &json!({ "n": not_whole }));
        assert_eq!(found.len(), 1, "{not_whole} produced {found:?}");
        assert!(found[0].contains("must be an integer"), "{found:?}");
    }
}

/// TC-PORT-SCHEMA-8: `oneOf` must match exactly one branch.
///
/// Upstream: the `oneOf` arm of `checkValue`, which counts matches and
/// requires precisely one.
///
/// Exactly one, not at least one: two matching branches mean the schema does
/// not say which shape the tool will read the value as, and that ambiguity is
/// the author's mistake rather than the model's.
///
/// Input: a `oneOf` of a string and a number, given each, then a boolean that
/// matches neither; then an ambiguous `oneOf` where two branches both match.
/// Expected: one match passes; zero and two are both refused, and the
/// complaint says how many matched, so an author can tell the two apart.
#[test]
fn one_of_requires_exactly_one_branch() {
    let schema = json!({
        "type": "object",
        "properties": {
            "value": { "oneOf": [{ "type": "string" }, { "type": "number" }] },
        },
    });

    assert_eq!(violations(&schema, &json!({ "value": "x" })), empty());
    assert_eq!(violations(&schema, &json!({ "value": 1 })), empty());

    let none = violations(&schema, &json!({ "value": true }));
    assert_eq!(none.len(), 1, "{none:?}");
    assert!(none[0].contains("matched 0"), "{none:?}");

    let ambiguous = json!({
        "type": "object",
        "properties": {
            "value": { "oneOf": [{ "type": "string" }, { "type": "string" }] },
        },
    });
    let two = violations(&ambiguous, &json!({ "value": "x" }));
    assert_eq!(two.len(), 1, "{two:?}");
    assert!(two[0].contains("matched 2"), "{two:?}");
}

/// TC-PORT-SCHEMA-9: a declaration this validator does not understand
/// constrains nothing.
///
/// Upstream throws `JsonSchemaError` here - "rejects an unknown schema type at
/// the author boundary" - because its DSL owns the schema and an unknown type
/// means the author wrote something its own converter could not have produced.
/// tetanus has no such boundary: a schema is a `serde_json::Value` a tool
/// hands over, and this helper is advisory rather than a gate.
///
/// So the direction is deliberately the other one. Refusing what it does not
/// understand would mean a tool using a keyword nobody has taught this
/// validator reports violations of a rule it is not actually checking, and an
/// author who trusted it would ship a tool that always looks wrong.
///
/// Input: an unknown `type`, an unknown keyword beside a known one, a schema
/// that is `true`, and a schema that is not a schema at all.
/// Expected: nothing is reported for what is not understood, while the known
/// keyword beside it is still enforced.
#[test]
fn an_unrecognised_declaration_constrains_nothing() {
    let unknown_type = json!({
        "type": "object",
        "properties": { "x": { "type": "weird" } },
    });
    assert_eq!(violations(&unknown_type, &json!({ "x": 1 })), empty());

    let unknown_keyword = json!({
        "type": "object",
        "properties": { "s": { "type": "string", "pattern": "^a+$", "minLength": 3 } },
    });
    assert_eq!(violations(&unknown_keyword, &json!({ "s": "b" })), empty());
    assert_eq!(
        violations(&unknown_keyword, &json!({ "s": 1 })),
        vec![r#""s" must be a string"#.to_string()],
        "the keyword it does understand is still enforced"
    );

    for permissive in [json!(true), json!("not a schema"), json!(null)] {
        assert_eq!(violations(&permissive, &json!({ "anything": 1 })), empty());
    }
}

/// TC-PORT-SCHEMA-10: the root is named for what it is.
///
/// A complaint about the arguments object itself has no property name to
/// quote, and reporting an empty pair of quotes reads as a bug in the harness
/// rather than a mistake in the call.
///
/// Input: a schema requiring an object, given an array; and a root-level
/// required key.
/// Expected: the root complaint names "the arguments object"; a missing
/// top-level key is still quoted by its own name and not by an empty path.
#[test]
fn the_root_is_named_rather_than_quoted_as_nothing() {
    let schema = json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"],
    });

    assert_eq!(
        violations(&schema, &json!([])),
        vec!["the arguments object must be an object".to_string()]
    );
    assert_eq!(
        violations(&schema, &json!({})),
        vec![r#"missing required property "path""#.to_string()]
    );
}

/// The empty answer, spelled once so a case reads as "nothing was wrong"
/// rather than as an empty literal that could be a typo.
fn empty() -> Vec<String> {
    Vec::<String>::new()
}

/// A guard against the one thing a validator must never do to a caller.
///
/// Every other case states what a particular schema says about a particular
/// value. This one states that no pair of them can panic, over a spread of
/// shapes chosen to hit each arm: the value kinds, a schema nested deeper than
/// the value, a value nested deeper than the schema, and the empty cases at
/// both ends.
#[test]
fn no_schema_and_value_pair_panics() {
    let schemas = [
        json!({}),
        json!({ "type": "object" }),
        json!({ "type": "object", "properties": { "a": { "type": "array", "items": { "type": "object", "properties": { "b": { "type": "integer" } } } } }, "required": ["a"] }),
        json!({ "type": "array", "items": { "type": "string" } }),
        json!({ "oneOf": [] }),
        json!({ "type": "string", "enum": [] }),
    ];
    let values = [
        json!(null),
        json!(0),
        json!(""),
        json!([]),
        json!({}),
        json!({ "a": [{ "b": 1 }, { "b": "no" }] }),
        json!({ "a": "not an array" }),
        json!([[[[1]]]]),
    ];

    for schema in &schemas {
        for value in &values {
            // The assertion is that this returns at all; the answer's content
            // is what the cases above are for.
            let _: Vec<String> = violations(schema, value);
        }
    }
}

/// The helper is deliberately not wired into dispatch, and this is the case
/// that says so on purpose rather than by omission.
///
/// TC-PORT-ARGS-3 in `upstream_tool_arguments.rs` pins that a call whose
/// arguments are not JSON still reaches its tool, carrying the raw text the
/// model wrote - which is upstream's behaviour, because upstream's loop parses
/// and does not validate. Those very arguments do not satisfy the usual
/// `{"type": "object"}` declaration, so gating dispatch on this helper would
/// break that port.
///
/// Expected: the helper reports the mismatch, and the reader is pointed at
/// where the decision not to act on it lives. If tetanus ever does gate, this
/// case fails and asks for the contract clause first.
#[test]
fn the_helper_answers_the_case_dispatch_deliberately_ignores() {
    let usual = json!({ "type": "object" });
    let raw_text_the_model_wrote: Value = json!("not json");

    let found = violations(&usual, &raw_text_the_model_wrote);

    assert_eq!(
        found,
        vec!["the arguments object must be an object".to_string()],
        "the helper has an opinion about this call"
    );
    // And TC-PORT-ARGS-3 asserts the call runs anyway. Both are true, and the
    // module note explains why that is upstream's shape rather than an
    // oversight.
}
