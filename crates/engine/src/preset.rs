//! Named agent presets: which model a session runs on, which tools it may
//! call, what its prompt opens with, and who it says it is.
//!
//! `tetanus_config::preset` finds presets - a directory per preset across
//! ordered roots - and `tetanus_config::preset::AgentPreset` reads what one
//! says. This is the step between that and a running session: the roster the
//! engine holds, the id a session was composed from, and the application of
//! one to a turn.
//!
//! **A preset is resolved once, at session creation, and never re-resolved.**
//! The id is written into the session header, so a session that was composed
//! from `fast` keeps running as `fast` even after the document changes - a
//! turn whose model changed under it half way through a conversation would
//! make the journal a record of two different agents. Upstream reaches the
//! same place with the same rule: its session preset is read from the
//! creation-time value, and only an explicit switch moves it.
//!
//! **A preset that is not there is a refusal, not a default.** A session asked
//! for by name is a session somebody meant; running it on the harness defaults
//! instead would silently give a model tools the preset was written to keep
//! away from it.
//!
//! **Two sources, one roster.** A preset may be written inline in the settings
//! document under `presets.<id>.<key>`, which is how a deployment with two
//! profiles avoids two directories, or as a directory holding its own settings
//! document under the roots `presets.roots` names. The inline definition wins,
//! for the reason the roots themselves are ordered: the nearer the harness,
//! the more trusted.

use std::collections::BTreeMap;

use tetanus_config::preset::{self, AgentPreset, Root};
use tetanus_config::{Config, ConfigError, Document};

/// The keys a document names presets with.
pub mod key {
    /// The preset a session with no other choice is composed from.
    pub const DEFAULT: &str = "presets.default";
    /// Directories holding one preset each, most trusted first.
    pub const ROOTS: &str = "presets.roots";
    /// The prefix an inline preset is written under: `presets.<id>.<key>`.
    pub const INLINE: &str = "presets.";
}

/// The keys under `presets.` that are not preset ids.
const RESERVED: [&str; 2] = ["default", "roots"];

/// Every preset this engine can compose a session from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roster {
    presets: BTreeMap<String, AgentPreset>,
    default: Option<String>,
}

impl Roster {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one preset under an id, replacing any held under the same id.
    pub fn with(mut self, id: impl Into<String>, preset: AgentPreset) -> Self {
        self.presets.insert(id.into(), preset);
        self
    }

    /// Name the preset a session with no choice of its own is composed from.
    pub fn defaulting_to(mut self, id: Option<String>) -> Self {
        self.default = id;
        self
    }

    pub fn ids(&self) -> Vec<String> {
        self.presets.keys().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.presets.is_empty()
    }

    /// The id a session with no explicit choice runs under.
    pub fn default_id(&self) -> Option<&str> {
        self.default.as_deref()
    }

    pub fn get(&self, id: &str) -> Option<&AgentPreset> {
        self.presets.get(id)
    }

    /// The preset a session named, or the default when it named none.
    ///
    /// An id nobody wrote is [`PresetError::Unknown`], which names what there
    /// is - a session composed from a typo would run on the harness defaults
    /// and look like a preset that does nothing.
    pub fn resolve(
        &self,
        asked: Option<&str>,
    ) -> Result<Option<(String, &AgentPreset)>, PresetError> {
        let Some(id) = asked.map(str::to_string).or_else(|| self.default.clone()) else {
            return Ok(None);
        };
        match self.presets.get(&id) {
            Some(preset) => Ok(Some((id, preset))),
            None => Err(PresetError::Unknown {
                id,
                known: self.ids(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PresetError {
    #[error("no preset called {id:?}; this harness composes: {}", listed(.known))]
    Unknown { id: String, known: Vec<String> },
}

fn listed(ids: &[String]) -> String {
    if ids.is_empty() {
        "(none)".to_string()
    } else {
        ids.join(", ")
    }
}

/// Read the roster out of the settings document, and out of whatever roots it
/// names.
pub fn roster(settings: &Config) -> Result<Roster, ConfigError> {
    let mut presets: BTreeMap<String, AgentPreset> = BTreeMap::new();

    // The directories first, so an inline definition of the same id wins.
    for root in roots(settings)? {
        for found in preset::discover(&[Root::user(root)])? {
            let Some(document) = found.health.settings() else {
                // A candidate that is not a working preset is the roster's to
                // report and this composer's to leave alone: an engine that
                // refused to boot because a directory somewhere held a typo
                // would be worse than one that composes the presets that work.
                continue;
            };
            let agent = AgentPreset::read(document)?;
            if !agent.is_empty() {
                presets.insert(found.id, agent);
            }
        }
    }

    for (id, document) in inline(settings) {
        let agent = AgentPreset::read(&document)?;
        if !agent.is_empty() {
            presets.insert(id, agent);
        }
    }

    let default = settings
        .get(key::DEFAULT)
        .and_then(|resolved| resolved.value.as_str())
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string);

    Ok(Roster { presets, default })
}

/// The preset roots a document names, in the order it named them.
fn roots(settings: &Config) -> Result<Vec<String>, ConfigError> {
    let Some(resolved) = settings.get(key::ROOTS) else {
        return Ok(Vec::new());
    };
    let listed = resolved
        .value
        .as_array()
        .ok_or_else(|| bad(key::ROOTS, "a list of directories", &resolved.value))?;
    listed
        .iter()
        .map(|root| match root.as_str() {
            Some(path) if !path.trim().is_empty() => Ok(path.to_string()),
            _ => Err(bad(key::ROOTS, "a list of directories", &resolved.value)),
        })
        .collect()
}

/// The presets written inline in the document, as one document each.
///
/// The layered config is flat dotted keys, so `presets.fast.model.default`
/// carries both which preset it belongs to and which key it sets. The id is
/// the first segment after the prefix, and the rest is the key the preset
/// document would have used - which is what makes an inline preset and a
/// preset directory the same vocabulary.
fn inline(settings: &Config) -> BTreeMap<String, Document> {
    let mut found: BTreeMap<String, Document> = BTreeMap::new();
    for (key, resolved) in settings.provenance() {
        let Some(rest) = key.strip_prefix(key::INLINE) else {
            continue;
        };
        let Some((id, inner)) = rest.split_once('.') else {
            continue;
        };
        if RESERVED.contains(&id) || id.is_empty() || inner.is_empty() {
            continue;
        }
        found
            .entry(id.to_string())
            .or_default()
            .insert(inner.to_string(), resolved.value.clone());
    }
    found
}

fn bad(key: &str, expected: &str, found: &serde_json::Value) -> ConfigError {
    ConfigError::BadValue {
        key: key.to_string(),
        expected: expected.to_string(),
        found: found.to_string(),
    }
}
