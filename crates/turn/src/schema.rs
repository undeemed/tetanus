//! Checking one tool call's arguments against the schema its tool published.
//!
//! A tool declares a JSON Schema so the model knows what to send. Nothing was
//! checking that what arrived matched it, so every tool body was on its own:
//! it either wrote the defensive parsing itself or trusted a value the model
//! made up. Central checking makes the schema mean something, and turns "the
//! model sent the wrong shape" into a message the model can read and act on
//! rather than whatever the body did with it.
//!
//! **This is a validator for the schemas tetanus tools publish, not a JSON
//! Schema implementation.** The supported vocabulary is exactly upstream's
//! (`packages/core/tools/src/json-schema.ts`): the six `type`s, `properties`,
//! `required`, `items`, `additionalProperties: false`, `enum`, `const` and
//! `oneOf`. Anything else in a schema is *ignored rather than refused*, which
//! is the deliberate direction: an unknown keyword must not make a working
//! tool uncallable, and a tool that needs a constraint this does not check can
//! still check it in its own body.
//!
//! Recursion is bounded by the *schema*, not by the value: the walk descends
//! only where the schema has a child node to descend into, so a deeply nested
//! argument the model invented cannot drive it any deeper than the tool's own
//! declaration goes.
//!
//! Parity: upstream `validateArgs`, pinned by its `schema.spec.ts`,
//! `json-schema.spec.ts` and the composition half of its `properties.spec.ts`.

use serde_json::Value;

/// Check `arguments` against `schema`, and answer every way it does not fit.
///
/// An empty answer means it fits. Every violation is reported rather than the
/// first, because a model that is told one mistake at a time needs one round
/// trip per mistake, and the whole point of answering it at all is that it can
/// correct itself.
pub fn violations(schema: &Value, arguments: &Value) -> Vec<String> {
    let mut found = Vec::new();
    check(schema, arguments, "", &mut found);
    found
}

fn check(schema: &Value, value: &Value, path: &str, found: &mut Vec<String>) {
    // A schema that is not an object constrains nothing. `true` is a valid
    // JSON Schema meaning "anything", and a malformed one is not the model's
    // mistake to be told about.
    let Some(node) = schema.as_object() else {
        return;
    };

    // `oneOf` is checked before `type`, as upstream does: a branch list is the
    // node's whole meaning, and exactly one branch must accept the value.
    if let Some(Value::Array(branches)) = node.get("oneOf") {
        let matched = branches
            .iter()
            .filter(|branch| violations(branch, value).is_empty())
            .count();
        if matched != 1 {
            found.push(format!(
                "{} must match exactly one oneOf branch (matched {matched})",
                at(path)
            ));
        }
        return;
    }

    let Some(ty) = node.get("type").and_then(Value::as_str) else {
        // No `type` is no constraint on the shape. `enum` and `const` may
        // still apply.
        scalar(node, value, path, found);
        return;
    };

    match ty {
        "object" => {
            let Some(members) = value.as_object() else {
                found.push(format!("{} must be an object", at(path)));
                return;
            };
            let properties = node.get("properties").and_then(Value::as_object);

            // A property present but null is absent for this purpose, which is
            // what upstream's `=== undefined` check amounts to: a model that
            // sends `{"path": null}` has not supplied a path.
            let supplied = |key: &str| members.get(key).is_some_and(|v| !matches!(v, Value::Null));

            if let Some(Value::Array(required)) = node.get("required") {
                for key in required.iter().filter_map(Value::as_str) {
                    if !supplied(key) {
                        found.push(format!("missing required property {}", quoted(path, key)));
                    }
                }
            }

            if let Some(properties) = properties {
                for (key, child) in properties {
                    if supplied(key) {
                        check(child, &members[key], &member(path, key), found);
                    }
                }
            }

            // Refusing an undeclared property is opt-in, because a tool that
            // did not say `additionalProperties: false` has not said extras
            // are wrong.
            if node.get("additionalProperties") == Some(&Value::Bool(false)) {
                let declared = properties;
                for key in members.keys() {
                    let known = declared.is_some_and(|d| d.contains_key(key));
                    if !known {
                        found.push(format!(
                            "{} is not a declared property (additionalProperties: false)",
                            quoted(path, key)
                        ));
                    }
                }
            }
        }
        "array" => {
            let Some(items) = value.as_array() else {
                found.push(format!("{} must be an array", at(path)));
                return;
            };
            if let Some(child) = node.get("items") {
                for (index, entry) in items.iter().enumerate() {
                    check(child, entry, &format!("{path}[{index}]"), found);
                }
            }
        }
        "string" => match value.as_str() {
            Some(_) => scalar(node, value, path, found),
            None => found.push(format!("{} must be a string", at(path))),
        },
        "number" => match value.as_f64() {
            // `as_f64` answers for every JSON number; a non-finite one cannot
            // be parsed from JSON in the first place, so reaching this is a
            // caller constructing a value by hand.
            Some(number) if number.is_finite() => scalar(node, value, path, found),
            _ => found.push(format!("{} must be a number", at(path))),
        },
        "integer" => {
            // A JSON `2.0` is an integer by value, which is what a model that
            // was told "integer" and produced a float meant. Refusing it would
            // fail a call over a spelling.
            let whole = value.as_i64().is_some()
                || value.as_u64().is_some()
                || value
                    .as_f64()
                    .is_some_and(|n| n.fract() == 0.0 && n.is_finite());
            match whole {
                true => scalar(node, value, path, found),
                false => found.push(format!("{} must be an integer", at(path))),
            }
        }
        "boolean" => match value.is_boolean() {
            true => scalar(node, value, path, found),
            false => found.push(format!("{} must be a boolean", at(path))),
        },
        "null" => match value.is_null() {
            true => scalar(node, value, path, found),
            false => found.push(format!("{} must be null", at(path))),
        },
        // A `type` this validator does not know constrains nothing, for the
        // reason the module note gives: an unrecognised declaration must not
        // make a working tool uncallable.
        _ => {}
    }
}

/// `enum` and `const`, which apply to whatever the value turned out to be.
fn scalar(
    node: &serde_json::Map<String, Value>,
    value: &Value,
    path: &str,
    found: &mut Vec<String>,
) {
    if let Some(Value::Array(allowed)) = node.get("enum") {
        if !allowed.contains(value) {
            found.push(format!(
                "{} must be one of {}",
                at(path),
                Value::Array(allowed.clone())
            ));
        }
    }
    if let Some(expected) = node.get("const") {
        if value != expected {
            found.push(format!("{} must be {expected}", at(path)));
        }
    }
}

/// How a path reads in a message. The root has no name, so it is named for
/// what it is rather than reported as an empty string.
fn at(path: &str) -> String {
    if path.is_empty() {
        "the arguments object".to_string()
    } else {
        format!("{path:?}")
    }
}

fn member(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn quoted(path: &str, key: &str) -> String {
    format!("{:?}", member(path, key))
}
