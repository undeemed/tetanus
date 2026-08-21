//! Named presets: settings a deployment can switch between by name.
//!
//! A preset is a directory holding a settings document. `fast` and `thorough`
//! are the obvious pair - a cheaper model with a short step budget, and an
//! expensive one with a long budget - and without them a deployment that wants
//! both has two copies of one document and edits whichever it remembers.
//!
//! **A roster is discovered, not configured.** Roots are searched in order and
//! every directory in them is a candidate, so adding a preset is adding a
//! directory. A list of preset names in the settings document would be a
//! second place to keep in step with the filesystem, and the two would drift.
//!
//! **The earlier root wins a duplicate id.** Roots are given most-trusted
//! first, so a deployment can ship a preset a user cannot silently replace by
//! creating a directory of the same name. The one that lost is not dropped
//! from the roster - it is recorded as shadowed, because "my preset does
//! nothing" is a question someone will ask and the answer should be readable.
//!
//! **A directory that is not a working preset is reported, not skipped.** A
//! typo in a document, or a directory nobody finished, is the case where
//! silence is worst: the preset simply does not appear, and its author has
//! nowhere to look. Every candidate comes back with its health, and a caller
//! decides whether to offer it.
//!
//! **A root that cannot be read is not an empty root.** Absence is a real
//! state and reads as no presets; a permission failure is a fault, because
//! treating it as empty would quietly serve a deployment none of the presets
//! it installed.
//!
//! Parity: upstream `packages/preset/agent-presets`, the discovery half of its
//! `discovery.spec.ts`. Its authoring half - copying a shipped preset into a
//! writable root, tightening modes, deleting - needs a write path this crate
//! does not have, and its composition health is about a Cordis plugin tree
//! rather than a settings document, so what is restated here is the roster.

use std::path::{Path, PathBuf};

use crate::{ConfigError, Document};

/// Where a preset came from, and therefore who may override whom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Trust {
    /// Installed with the harness.
    Shipped,
    /// Written by whoever runs it.
    User,
}

/// Whether a candidate is usable, and why not when it is not.
#[derive(Debug, Clone, PartialEq)]
pub enum Health {
    /// The document read, and is the settings this preset applies.
    Ready(Document),
    /// A directory with no settings document in it.
    NoDocument,
    /// A document that is there and will not read.
    Unreadable(String),
    /// A preset of the same id was found in an earlier, more trusted root.
    Shadowed { by: Trust },
}

impl Health {
    /// Whether this preset can actually be applied.
    pub fn is_ready(&self) -> bool {
        matches!(self, Health::Ready(_))
    }

    /// The settings it applies, when it has any.
    pub fn settings(&self) -> Option<&Document> {
        match self {
            Health::Ready(document) => Some(document),
            _ => None,
        }
    }
}

/// One candidate found in a root.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub id: String,
    pub trust: Trust,
    /// The directory it was found in, so a message can name where to look.
    pub path: PathBuf,
    pub health: Health,
}

/// A root to search, and how far its presets are trusted.
#[derive(Debug, Clone)]
pub struct Root {
    pub trust: Trust,
    pub path: PathBuf,
}

impl Root {
    pub fn shipped(path: impl Into<PathBuf>) -> Self {
        Self {
            trust: Trust::Shipped,
            path: path.into(),
        }
    }

    pub fn user(path: impl Into<PathBuf>) -> Self {
        Self {
            trust: Trust::User,
            path: path.into(),
        }
    }
}

/// Whether a directory name is a name a preset may have.
///
/// The same character set the rest of this workspace accepts for an
/// identifier, so a preset id is safe in a path, a settings key and a command
/// line at once. A directory that cannot be named is not an error - a root may
/// hold anything - it is simply not a preset.
pub fn is_preset_id(name: &str) -> bool {
    // A leading dot is a hidden directory by convention, never a preset.
    // Without this `.git`, `.svn` and `.DS_Store` are all perfectly good ids
    // and every roster in a version-controlled preset root reports them as
    // broken slots.
    (1..=64).contains(&name.len())
        && !name.starts_with('.')
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Find every preset in `roots`, most-trusted root first.
///
/// The answer is ordered by id, so a listing is stable whatever order a
/// filesystem hands its entries back in - which is not an order anyone should
/// depend on and differs between systems.
pub fn discover(roots: &[Root]) -> Result<Vec<Preset>, ConfigError> {
    let mut found: Vec<Preset> = Vec::new();

    for root in roots {
        for path in directories(&root.path)? {
            let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !is_preset_id(id) {
                continue;
            }
            // An id already claimed by an earlier root keeps that root's
            // preset; this one is recorded so its author can see why it is not
            // being used.
            if let Some(winner) = found.iter().find(|held| held.id == id) {
                let by = winner.trust;
                found.push(Preset {
                    id: id.to_string(),
                    trust: root.trust,
                    path: path.clone(),
                    health: Health::Shadowed { by },
                });
                continue;
            }
            found.push(Preset {
                id: id.to_string(),
                trust: root.trust,
                health: health_of(&path),
                path,
            });
        }
    }

    // By id, then by trust, so a shadowed duplicate sorts beside the preset
    // that beat it rather than somewhere else in the list.
    found.sort_by(|a, b| a.id.cmp(&b.id).then(a.trust.cmp(&b.trust)));
    Ok(found)
}

/// The presets a caller can actually apply, by id.
pub fn ready(roots: &[Root]) -> Result<Vec<Preset>, ConfigError> {
    Ok(discover(roots)?
        .into_iter()
        .filter(|preset| preset.health.is_ready())
        .collect())
}

/// Find one preset by id.
pub fn find(roots: &[Root], id: &str) -> Result<Option<Preset>, ConfigError> {
    Ok(discover(roots)?
        .into_iter()
        .find(|preset| preset.id == id && !matches!(preset.health, Health::Shadowed { .. })))
}

/// The directories directly inside a root, sorted by name.
///
/// An absent root is no presets. Anything else that stops the read is a fault:
/// a root the process may not read holds presets it cannot see, and answering
/// "none" would serve a deployment a configuration it did not ask for.
fn directories(root: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ConfigError::Unreadable {
                path: root.to_path_buf(),
                source,
            })
        }
    };

    let mut directories: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ConfigError::Unreadable {
            path: root.to_path_buf(),
            source,
        })?;
        // A plain file beside the preset directories is not a preset, and is
        // not a mistake either - a root may hold a README.
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            directories.push(entry.path());
        }
    }
    directories.sort();
    Ok(directories)
}

/// Read a candidate directory's settings document.
fn health_of(directory: &Path) -> Health {
    let mut document: Option<PathBuf> = None;
    for extension in crate::file::EXTENSIONS {
        let candidate = directory.join(format!("settings.{extension}"));
        if candidate.is_file() {
            document = Some(candidate);
            break;
        }
    }
    let Some(document) = document else {
        return Health::NoDocument;
    };
    match crate::file::read(&document) {
        Ok(settings) => Health::Ready(settings),
        // The reader's own words: it already names the file and says what is
        // wrong with it, and rewording that here would lose the detail.
        Err(fault) => Health::Unreadable(fault.to_string()),
    }
}

/// What a preset says an agent is.
///
/// A preset directory holds an ordinary settings document, and these are the
/// keys an *agent* preset sets in it. Every one is optional: a preset that
/// names only a model is a perfectly good preset, and what it does not say is
/// inherited from the harness it is applied to.
///
/// The keys are the ones the rest of the workspace already uses for the same
/// things (`model.default`, `agent.max_steps`), plus two that only a preset
/// sets: the tool subset and the persona. That is deliberate - a preset
/// document a user can also use as a whole settings document is one fewer
/// vocabulary to learn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentPreset {
    /// The model a session composed from this preset runs on.
    pub model: Option<String>,
    /// The provider route that model is reached over.
    pub provider: Option<String>,
    /// The step budget.
    pub max_steps: Option<u32>,
    /// The tools this agent may call. `None` is every tool the harness has;
    /// an empty list is an agent with no tools, which is a thing to ask for.
    pub tools: Option<Vec<String>>,
    /// The opening system-prompt section - the shape of the prompt rather
    /// than its whole text, since plugins still contribute their sections.
    pub prompt: Option<String>,
    /// Who the agent is, as one section the deployment's own text goes in.
    pub persona: Option<String>,
}

/// The keys an agent preset is written with.
pub mod agent_key {
    pub const MODEL: &str = "model.default";
    pub const PROVIDER: &str = "provider.default";
    pub const MAX_STEPS: &str = "agent.max_steps";
    pub const TOOLS: &str = "agent.tools";
    pub const PROMPT: &str = "agent.prompt";
    pub const PERSONA: &str = "agent.persona";
}

impl AgentPreset {
    /// Read one from a settings document.
    ///
    /// A value of the wrong type is refused rather than ignored, for the
    /// reason [`crate::ConfigError::BadValue`] exists: a preset that quietly
    /// dropped its model would run a session on a model nobody chose.
    pub fn read(document: &Document) -> Result<Self, ConfigError> {
        Ok(Self {
            model: text(document, agent_key::MODEL)?,
            provider: text(document, agent_key::PROVIDER)?,
            max_steps: steps(document, agent_key::MAX_STEPS)?,
            tools: names(document, agent_key::TOOLS)?,
            prompt: text(document, agent_key::PROMPT)?,
            persona: text(document, agent_key::PERSONA)?,
        })
    }

    /// Whether this preset says anything at all. A directory whose document
    /// sets none of these keys is a settings preset, not an agent one.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// A key holding text, which is never blank when it is present.
fn text(document: &Document, key: &str) -> Result<Option<String>, ConfigError> {
    let Some(value) = document.get(key) else {
        return Ok(None);
    };
    match value.as_str() {
        Some(text) if !text.trim().is_empty() => Ok(Some(text.to_string())),
        _ => Err(ConfigError::BadValue {
            key: key.to_string(),
            expected: "text with something in it".to_string(),
            found: value.to_string(),
        }),
    }
}

fn steps(document: &Document, key: &str) -> Result<Option<u32>, ConfigError> {
    let Some(value) = document.get(key) else {
        return Ok(None);
    };
    match value
        .as_u64()
        .filter(|steps| (1..=u32::MAX as u64).contains(steps))
    {
        Some(steps) => Ok(Some(steps as u32)),
        None => Err(ConfigError::BadValue {
            key: key.to_string(),
            expected: "a whole number of steps, one or more".to_string(),
            found: value.to_string(),
        }),
    }
}

/// A key holding a list of tool names.
///
/// An element that is not a name fails the whole list rather than dropping out
/// of it, exactly as a configured tool order does: a list quietly one entry
/// shorter is an agent missing a tool nobody took away.
fn names(document: &Document, key: &str) -> Result<Option<Vec<String>>, ConfigError> {
    let Some(value) = document.get(key) else {
        return Ok(None);
    };
    let listed = value.as_array().ok_or_else(|| ConfigError::BadValue {
        key: key.to_string(),
        expected: "a list of tool names".to_string(),
        found: value.to_string(),
    })?;
    listed
        .iter()
        .map(|name| match name.as_str() {
            Some(name) if !name.trim().is_empty() => Ok(name.trim().to_string()),
            _ => Err(ConfigError::BadValue {
                key: key.to_string(),
                expected: "a list of tool names".to_string(),
                found: value.to_string(),
            }),
        })
        .collect::<Result<Vec<String>, ConfigError>>()
        .map(Some)
}
