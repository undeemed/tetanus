//! The order the model reads its tools in, resolved out of the settings
//! document.
//!
//! [`tetanus_turn::tools::ToolOrder`] owns the rule - the rest entry, the
//! names, and the check against the registry the order arranges. What was
//! missing was the step before it, so an order was a value a composer passed
//! in and never a value anybody could configure. This module is that step,
//! beside [`crate::retry`] and for the same reason: an order each surface
//! resolved for itself is an order two surfaces can disagree about.
//!
//! Parity: upstream reads the same list from its system-prompt plugin's
//! configuration (`toolOrder`, `packages/core/system-prompt`). One difference,
//! recorded in `docs/parity.md`: the names are checked against the registry
//! here, before an engine exists, rather than while a turn assembles its
//! prompt.

use serde_json::Value;
use tetanus_config::{Config, ConfigError, Document};
use tetanus_turn::tools::{ToolOrder, ToolRegistry, TOOL_ORDER_REST};

use crate::boot::bad;

/// The key a document names an order with.
pub mod key {
    pub const ORDER: &str = "tools.order";
}

/// The compiled default as a layer document: no order at all.
///
/// An empty list is the default rather than a mistake, and it is the one list
/// that cannot mean anything else - an order that arranges nothing still needs
/// the rest entry, so `[]` can only be "no order named".
pub fn defaults() -> Document {
    Document::from([(key::ORDER.to_string(), Value::Array(Vec::new()))])
}

/// The order `settings` names, read against `registry`, or `None` when it
/// names none.
///
/// The check happens here because `registry` is settled before any turn runs.
/// A caller that swaps the registry afterwards has changed what the order was
/// read against, and resolves it again against the registry it will use.
pub fn order(settings: &Config, registry: &ToolRegistry) -> Result<Option<ToolOrder>, ConfigError> {
    let Some(resolved) = settings.get(key::ORDER) else {
        return Ok(None);
    };
    let Some(names) = names(&resolved.value) else {
        return Err(bad(
            key::ORDER,
            &format!("a list of tool names, one of them {TOOL_ORDER_REST:?}"),
            &resolved.value,
        ));
    };
    if names.is_empty() {
        return Ok(None);
    }
    // The rule is the turn crate's, so a document and a composer are refused
    // for the same reasons and read the same message.
    ToolOrder::new(names, registry)
        .map(Some)
        .map_err(|refused| {
            bad(
                key::ORDER,
                &format!("an order this engine can run ({refused})"),
                &resolved.value,
            )
        })
}

/// The names in a value that is a list of names, or `None` for a value that is
/// something else. An element that is not a name fails the whole list rather
/// than dropping out of it: a list quietly one entry shorter than it was
/// written is a tool the model reads in a place nobody chose.
fn names(value: &Value) -> Option<Vec<String>> {
    let listed = value.as_array()?;
    listed
        .iter()
        .map(|name| match name.as_str() {
            Some(name) if !name.trim().is_empty() => Some(name.to_string()),
            _ => None,
        })
        .collect()
}
