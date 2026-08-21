//! What a namespace claims about its own keys.
//!
//! Until now the settings document was read with nothing to check it against.
//! That has three consequences, and this module is the answer to all three
//! because they are one missing thing rather than three.
//!
//! **A scalar written where a section belongs was ignored.** `llm: off` in the
//! document contributed the key `llm`, which no reader claims, while every
//! `llm.*` key went on resolving from the layer below - so a user who thought
//! they had turned something off had changed nothing at all, and nothing said
//! so. Refusing it needs somebody to know that `llm` is a section, which is
//! exactly what a schema is. `docs/parity.md` carried this as the open question
//! behind TC-PORT-SET-5.
//!
//! **A credential was hidden by the spelling of its key.** `secret::names_a_secret`
//! reads the last word of a key, because with no schema the name was all there
//! was. It is a good heuristic and it is still the fallback, but a heuristic
//! decides wrongly in both directions: a key called `deploy.token_count` is
//! hidden and a key called `llm.deepseek.auth` is published. A namespace that
//! declares which of its keys hold credentials is not guessing.
//!
//! **A value of the wrong type reached the reader that wanted it.** A budget
//! written as `"eight"` was a `BadValue` discovered by whichever component
//! happened to read it first, mid-run, if anything read it at all.
//!
//! **A key no schema claims is still allowed.** This is deliberate and it is
//! what keeps the schema from becoming a second place every plugin must
//! register before it can be configured: an unclaimed key resolves as it always
//! did, and only a *conflict with a declared shape* is refused. A schema here
//! narrows what can go wrong; it is not a whitelist of what may exist.
//!
//! Parity: upstream `packages/settings/settings`, whose per-namespace
//! schemastery schemas decide the same three things
//! (`installSettingsSection`, `redactSecrets`).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{ConfigError, Document};

/// What one declared key holds.
///
/// Deliberately coarse. The question a settings schema has to answer is "is
/// this the kind of thing that key takes", and a richer vocabulary - ranges,
/// patterns, enums - is a validator each reader can apply to a value it already
/// has, whereas none of them can retrofit the *shape* rules above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Boolean,
    Integer,
    Number,
    Text,
    /// A list. Its members are not checked here: a config key holds a list as
    /// one value, and a reader that cares about the members reads them.
    List,
    /// Anything at all. For a key whose shape is a reader's business, declared
    /// so the key can still be marked secret or documented.
    Any,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "a boolean",
            Self::Integer => "a whole number",
            Self::Number => "a number",
            Self::Text => "text",
            Self::List => "a list",
            Self::Any => "any value",
        }
    }

    /// Whether `value` is this kind.
    ///
    /// An integer written `2.0` is an integer, for the reason
    /// `tetanus_turn::schema` gives about the same case: a document that spells
    /// a whole number with a decimal point has still said the number.
    pub fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Boolean => value.is_boolean(),
            Self::Integer => {
                value.as_i64().is_some()
                    || value.as_u64().is_some()
                    || value
                        .as_f64()
                        .is_some_and(|n| n.fract() == 0.0 && n.is_finite())
            }
            Self::Number => value.as_f64().is_some_and(f64::is_finite),
            Self::Text => value.is_string(),
            Self::List => value.is_array(),
            Self::Any => true,
        }
    }
}

/// One declared key.
#[derive(Debug, Clone)]
pub struct Field {
    pub kind: Kind,
    /// Whether the value is a credential. A declared field decides this
    /// outright; a key no field claims falls back to
    /// [`crate::secret::names_a_secret`].
    pub secret: bool,
    /// One line for a reader of the published catalogue. Empty when the
    /// namespace did not write one.
    pub description: String,
}

impl Field {
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            secret: false,
            description: String::new(),
        }
    }

    /// Mark the field a credential: its value is never published.
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    pub fn describing(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

/// Every namespace's declaration, together.
///
/// Flat, keyed by the dotted key a document resolves to, because that is what
/// the rest of this crate speaks. A "namespace" is therefore a prefix rather
/// than a nested object, which keeps one lookup where upstream needs a walk.
#[derive(Debug, Default, Clone)]
pub struct Schema {
    fields: BTreeMap<String, Field>,
}

impl Schema {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare one key.
    pub fn declare(&mut self, key: impl Into<String>, field: Field) -> &mut Self {
        self.fields.insert(key.into(), field);
        self
    }

    /// Declare one key, for a builder-style composition.
    pub fn with(mut self, key: impl Into<String>, field: Field) -> Self {
        self.declare(key, field);
        self
    }

    pub fn field(&self, key: &str) -> Option<&Field> {
        self.fields.get(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.fields.keys()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Whether any declared key sits under `key.` - which is what makes `key`
    /// a section rather than a value.
    ///
    /// The dot matters: `llm` is a section when `llm.model` is declared, and
    /// `llm_mode` is not made a section by it.
    pub fn is_section(&self, key: &str) -> bool {
        let prefix = format!("{key}.");
        self.fields
            .keys()
            .any(|declared| declared.starts_with(&prefix))
    }

    /// Whether this key must never be published.
    ///
    /// A declaration decides; a key nothing declares falls back to the name
    /// heuristic, so a document holding a credential under a key no namespace
    /// claims is still not printed. Failing safe in the direction of hiding is
    /// the only defensible default: publishing a credential cannot be undone,
    /// and hiding a value the user can read from their own file costs them one
    /// `cat`.
    pub fn is_secret(&self, key: &str) -> bool {
        match self.field(key) {
            Some(field) => field.secret,
            None => crate::secret::names_a_secret(key),
        }
    }

    /// Check one flat document against this schema.
    ///
    /// Every violation is reported rather than the first, for the reason a
    /// tool's argument validator reports all of them: a user fixing a settings
    /// file one message at a time needs one run of the harness per mistake.
    pub fn check(&self, document: &Document) -> Vec<ConfigError> {
        let mut faults = Vec::new();
        for (key, value) in document {
            // A scalar at a key that other keys live under is the case this
            // module exists for: the write means something the flat model
            // cannot express, and half-applying it is worse than refusing it.
            if self.is_section(key) {
                faults.push(ConfigError::SectionExpected {
                    key: key.clone(),
                    found: describe(value),
                });
                continue;
            }
            if let Some(field) = self.field(key) {
                if !field.kind.accepts(value) {
                    faults.push(ConfigError::BadValue {
                        key: key.clone(),
                        expected: field.kind.as_str().to_string(),
                        found: describe(value),
                    });
                }
            }
        }
        faults
    }

    /// Check a document and answer it, or the first fault with the rest
    /// attached to its message.
    ///
    /// The convenience form for a caller that loads a layer and has one place
    /// to report an error. [`Schema::check`] is what a caller that can show a
    /// list uses.
    pub fn accept(&self, document: Document) -> Result<Document, ConfigError> {
        let mut faults = self.check(&document);
        if faults.is_empty() {
            return Ok(document);
        }
        let first = faults.remove(0);
        if faults.is_empty() {
            return Err(first);
        }
        Err(ConfigError::Rejected {
            first: first.to_string(),
            others: faults.iter().map(ToString::to_string).collect(),
        })
    }
}

/// How a value reads in a message: what it is, not what it holds. A user who
/// wrote a credential in the wrong place should not find it echoed in an error.
fn describe(value: &Value) -> String {
    match value {
        Value::Null => "nothing".into(),
        Value::Bool(_) => "a boolean".into(),
        Value::Number(_) => "a number".into(),
        Value::String(_) => "text".into(),
        Value::Array(items) => format!("a list of {}", items.len()),
        Value::Object(fields) => format!("a section of {}", fields.len()),
    }
}
