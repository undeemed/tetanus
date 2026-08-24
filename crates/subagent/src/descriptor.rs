//! The durable record that says what a child is.
//!
//! Every session-backed child writes one `subagent/descriptor` into its own
//! journal, inside its first turn. It answers two questions nothing else can
//! after the fact: which provider established this child, and whether the
//! child is a one-shot run or a conversation that can be resumed cold.
//!
//! # Why the record snapshots named fields instead of the options object
//!
//! A child is composed from an options object that other extensions may add
//! to. Persisting that object wholesale would mean an unrelated extension's
//! value — possibly not even JSON — could make a resume fail. So the record
//! carries an explicit, closed list of fields, and supporting a new
//! composition input is a deliberate [`SUBAGENT_DESCRIPTOR_VERSION`] change
//! rather than an extra key appearing.
//!
//! Two things are deliberately *not* in it:
//!
//! - the delegation depth, because cold resume trusts the persisted header's
//!   value as the monotone floor (see [`crate::depth`]); and
//! - per-activation budgets like an output schema or a token cap, because
//!   those bound one activation rather than describing the child.
//!
//! # Unknown fields are refused, unknown versions are not
//!
//! These are different failures and get different answers. A record whose
//! version this build does not know means "a newer runtime wrote this child" —
//! it is not classifiable here, so it reads as absent and the caller treats
//! the child as unrecognised. A record at *this* version carrying a field this
//! version does not declare is a corrupt or hand-edited record, and reading it
//! would mean silently ignoring composition someone asked for.
//!
//! Parity: upstream `packages/subagent/subagent/src/descriptor.ts`.

use serde_json::{Map, Value};
use tetanus_session::SessionEvent;

/// The journal record this module reads and writes.
pub const DESCRIPTOR_EVENT: &str = "subagent/descriptor";

/// The format version stamped into every record, and required verbatim when
/// one is read back.
pub const SUBAGENT_DESCRIPTOR_VERSION: u64 = 2;

/// Whether a child is a terminal run or a resumable conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentMode {
    /// It runs once and is done.
    OneShot,
    /// It can be resumed later, from the journal alone.
    Continuable,
}

impl SubagentMode {
    /// The word on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            SubagentMode::OneShot => "one-shot",
            SubagentMode::Continuable => "continuable",
        }
    }
}

/// Which tools a child may use.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolFilter {
    /// If present, only these.
    pub allow: Option<Vec<String>>,
    /// If present, never these.
    pub deny: Option<Vec<String>>,
}

/// What a descriptor says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentDescriptor {
    /// One-shot or continuable.
    pub mode: SubagentMode,
    /// The provider that established the child.
    pub provider: String,
    /// The child's durable label. Required for a continuable child, because
    /// enumeration must be able to name it without replaying the parent's
    /// tool results or exposing the child's prompt.
    pub label: Option<String>,
    /// Composition reapplied on resume. Only a continuable child has any.
    pub agent_provider: Option<String>,
    /// As above.
    pub agent_model: Option<String>,
    /// As above.
    pub persona: Option<String>,
    /// As above.
    pub tool_filter: Option<ToolFilter>,
}

/// A persisted record that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("persisted subagent descriptor {0}")]
pub struct DescriptorError(pub String);

fn bad(message: impl Into<String>) -> DescriptorError {
    DescriptorError(message.into())
}

/// Fields a record at this version may carry, by mode.
fn declared_keys(mode: SubagentMode) -> &'static [&'static str] {
    match mode {
        SubagentMode::OneShot => &["version", "mode", "provider", "label"],
        SubagentMode::Continuable => &[
            "version",
            "mode",
            "provider",
            "label",
            "agentProvider",
            "agentModel",
            "persona",
            "toolFilter",
        ],
    }
}

/// The descriptor a child's journal carries, if it has a readable one.
///
/// `None` means either no record or one written by a version this build does
/// not know. `Err` means a record at this version that does not match this
/// version's shape.
pub fn fold_descriptor(
    events: &[SessionEvent],
) -> Result<Option<SubagentDescriptor>, DescriptorError> {
    let Some(event) = events.iter().find(|event| event.ty == DESCRIPTOR_EVENT) else {
        return Ok(None);
    };
    parse_descriptor(&event.data)
}

/// Read one persisted payload.
pub fn parse_descriptor(value: &Value) -> Result<Option<SubagentDescriptor>, DescriptorError> {
    let record = value
        .as_object()
        .ok_or_else(|| bad("payload must be an object"))?;

    let version = record
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad("version must be a number"))?;
    // A newer runtime wrote this child. Not classifiable here, and not an
    // error: absent is the honest answer.
    if version != SUBAGENT_DESCRIPTOR_VERSION {
        return Ok(None);
    }

    let mode = match record.get("mode").and_then(Value::as_str) {
        Some("one-shot") => SubagentMode::OneShot,
        Some("continuable") => SubagentMode::Continuable,
        _ => return Err(bad("mode must be \"one-shot\" or \"continuable\"")),
    };
    assert_known_keys(record, declared_keys(mode), "payload")?;

    let provider = record
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("provider must be a string"))?
        .to_owned();

    let mut descriptor = SubagentDescriptor {
        mode,
        provider,
        label: optional_string(record, "label")?,
        agent_provider: None,
        agent_model: None,
        persona: None,
        tool_filter: None,
    };

    if mode == SubagentMode::OneShot {
        return Ok(Some(descriptor));
    }

    // A continuable child must be nameable, so its label is required rather
    // than optional.
    if descriptor.label.is_none() {
        return Err(bad("label must be a string"));
    }
    descriptor.agent_provider = optional_string(record, "agentProvider")?;
    descriptor.agent_model = optional_string(record, "agentModel")?;
    descriptor.persona = optional_string(record, "persona")?;
    descriptor.tool_filter = match record.get("toolFilter") {
        Some(value) => Some(parse_tool_filter(value)?),
        None => None,
    };
    Ok(Some(descriptor))
}

/// Build the payload for a new record.
pub fn descriptor_payload(descriptor: &SubagentDescriptor) -> Value {
    let mut payload = Map::new();
    payload.insert("version".into(), Value::from(SUBAGENT_DESCRIPTOR_VERSION));
    payload.insert("mode".into(), Value::from(descriptor.mode.as_str()));
    payload.insert("provider".into(), Value::from(descriptor.provider.clone()));
    insert_some(&mut payload, "label", descriptor.label.clone());

    if descriptor.mode == SubagentMode::Continuable {
        insert_some(
            &mut payload,
            "agentProvider",
            descriptor.agent_provider.clone(),
        );
        insert_some(&mut payload, "agentModel", descriptor.agent_model.clone());
        insert_some(&mut payload, "persona", descriptor.persona.clone());
        if let Some(filter) = &descriptor.tool_filter {
            let mut written = Map::new();
            if let Some(allow) = &filter.allow {
                written.insert("allow".into(), Value::from(allow.clone()));
            }
            if let Some(deny) = &filter.deny {
                written.insert("deny".into(), Value::from(deny.clone()));
            }
            payload.insert("toolFilter".into(), Value::Object(written));
        }
    }
    Value::Object(payload)
}

/// A field that is absent is omitted, never written as null.
fn insert_some(payload: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        payload.insert(key.to_owned(), Value::from(value));
    }
}

/// Refuse a field this version does not declare. See the module note: at the
/// current version, an extra key is corruption, not forward compatibility.
fn assert_known_keys(
    record: &Map<String, Value>,
    declared: &[&str],
    path: &str,
) -> Result<(), DescriptorError> {
    match record.keys().find(|key| !declared.contains(&key.as_str())) {
        Some(unknown) => Err(bad(format!("{path} has unknown field \"{unknown}\""))),
        None => Ok(()),
    }
}

/// A field that is present must be a string; absent is fine.
fn optional_string(
    record: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, DescriptorError> {
    match record.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(bad(format!("{key} must be a string"))),
    }
}

/// A tool restriction that says nothing is a mistake, not a permissive filter.
fn parse_tool_filter(value: &Value) -> Result<ToolFilter, DescriptorError> {
    let record = value
        .as_object()
        .ok_or_else(|| bad("toolFilter must be an object"))?;
    assert_known_keys(record, &["allow", "deny"], "toolFilter")?;

    let filter = ToolFilter {
        allow: optional_string_array(record, "allow")?,
        deny: optional_string_array(record, "deny")?,
    };
    if filter.allow.is_none() && filter.deny.is_none() {
        return Err(bad("toolFilter must declare allow and/or deny"));
    }
    Ok(filter)
}

/// A present list must be entirely strings.
fn optional_string_array(
    record: &Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>, DescriptorError> {
    let Some(value) = record.get(key) else {
        return Ok(None);
    };
    let message = format!("toolFilter.{key} must be an array of strings");
    let items = value.as_array().ok_or_else(|| bad(message.clone()))?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| bad(message.clone()))
        })
        .collect::<Result<Vec<String>, _>>()
        .map(Some)
}
