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
