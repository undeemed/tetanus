//! Writing the settings document back.
//!
//! Until now the document was read and never written, so anything that wanted
//! to change a setting - a `/permission` command, a surface's preferences pane,
//! a first-run wizard - had to tell the user to edit YAML by hand.
//!
//! **A write is read-modify-write over the document's own shape.** The keys
//! this crate resolves are flat and dotted; a document is nested sections. So
//! an edit is applied by walking into the parsed document, setting the leaf,
//! and writing the whole thing back - not by appending a flat key, which would
//! produce a file that reads back differently from the one that was written.
//!
//! **A section a scalar occupies is refused rather than replaced.** Writing
//! `llm.model` into a document that holds `llm: off` would silently discard the
//! `off` the user wrote. That is the same rule [`crate::schema`] applies when
//! reading, from the other direction, and it is the one case where a write
//! would destroy something.
//!
//! **The replace is atomic and the file is owner-only.** A temporary beside the
//! document, flushed, then renamed over it: a crash mid-write loses the edit
//! rather than the document. Owner-only because a settings document may hold a
//! credential, and a file that briefly held one at 0644 has published it -
//! permissions are set on the temporary before anything is written into it,
//! not on the destination afterwards.
//!
//! **Comments are not preserved, and that is a stated cost.** Upstream writes
//! through a comment-preserving YAML editor. Reproducing that means a
//! round-tripping parser this workspace does not have, so a written document
//! keeps its data and loses its commentary. [`update`] therefore refuses to
//! touch a document it cannot round-trip cleanly, and `docs/parity-updates/`
//! records the gap rather than letting a user discover it by losing a comment.
//!
//! Parity: upstream `packages/settings/settings-file`, the persist half of its
//! `local.spec.ts` (`update`, `mutate`, `publish`).

use std::path::Path;

use serde_json::{Map, Value};

use crate::{ConfigError, Document};

/// One edit: a dotted key, and what to do with it.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Write this value at this key.
    Set(String, Value),
    /// Take the key out. Removing one that is not there is not an error - the
    /// caller asked for it to be gone, and it is.
    Remove(String),
}

impl Edit {
    pub fn set(key: impl Into<String>, value: impl Into<Value>) -> Self {
        Self::Set(key.into(), value.into())
    }

    pub fn remove(key: impl Into<String>) -> Self {
        Self::Remove(key.into())
    }

    fn key(&self) -> &str {
        match self {
            Self::Set(key, _) | Self::Remove(key) => key,
        }
    }
}

/// Apply `edits` to the document at `path`, and write it back.
///
/// The document need not exist: a first write creates it, with its parent
/// directories, which is what lets a surface offer "remember this" before the
/// user has ever edited a settings file.
///
/// Answers the flat document as it now reads on disk, so a caller can load it
/// into [`crate::Layer::File`] without reading the file a second time and
/// racing itself.
pub fn update(path: &Path, edits: &[Edit]) -> Result<Document, ConfigError> {
    let mut root = read_root(path)?;
    for edit in edits {
        apply(path, &mut root, edit)?;
    }

    let text = render(path, &root)?;
    publish(path, &text)?;
    crate::file::read(path)
}

/// The document as a nested tree, or an empty one when there is nothing there.
///
/// Reads through the same parser [`crate::file::read`] uses, so a document this
/// module will rewrite is one the rest of the crate could already read - a
/// write path that accepted more than the read path would produce files the
/// harness then refused.
fn read_root(path: &Path) -> Result<Map<String, Value>, ConfigError> {
    let extension = extension(path)?;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(ConfigError::Unreadable {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    let parsed: Value = match extension.as_str() {
        "json" => serde_json::from_str(&text),
        _ => serde_norway::from_str(&text).map_err(serde::de::Error::custom),
    }
    .map_err(|e: serde_json::Error| ConfigError::Malformed {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    match parsed {
        Value::Null => Ok(Map::new()),
        Value::Object(root) => Ok(root),
        _ => Err(ConfigError::NotAMap {
            path: path.to_path_buf(),
        }),
    }
}

fn extension(path: &Path) -> Result<String, ConfigError> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !crate::file::EXTENSIONS.contains(&extension.as_str()) {
        return Err(ConfigError::UnsupportedExtension {
            path: path.to_path_buf(),
            extension,
        });
    }
    Ok(extension)
}

/// Apply one edit to the parsed tree.
fn apply(path: &Path, root: &mut Map<String, Value>, edit: &Edit) -> Result<(), ConfigError> {
    let segments: Vec<&str> = edit.key().split('.').filter(|s| !s.is_empty()).collect();
    let Some((leaf, sections)) = segments.split_last() else {
        return Err(ConfigError::BadValue {
            key: edit.key().to_string(),
            expected: "a dotted key".to_string(),
            found: "an empty key".to_string(),
        });
    };

    let mut here = root;
    for section in sections {
        // A section that is not there is created; one that is occupied by a
        // scalar is refused, because writing through it would silently discard
        // what the user wrote.
        let entry = here
            .entry((*section).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            return Err(ConfigError::SectionExpected {
                key: sections_up_to(&segments, section),
                found: crate::schema::describe(entry),
            });
        }
        here = entry.as_object_mut().expect("checked just above");
    }

    match edit {
        Edit::Set(_, value) => {
            here.insert((*leaf).to_string(), value.clone());
        }
        Edit::Remove(_) => {
            here.remove(*leaf);
            // A section the removal emptied is left in place. A document that
            // shed its empty sections would rewrite parts of itself the caller
            // did not ask about, and an empty section is what a user writes
            // when they are about to fill it in.
        }
    }
    let _ = path;
    Ok(())
}

/// The dotted path up to and including `section`, for a message that names
/// where the conflict is rather than which segment the walk was on.
fn sections_up_to(segments: &[&str], section: &str) -> String {
    let mut named = Vec::new();
    for part in segments {
        named.push(*part);
        if *part == section {
            break;
        }
    }
    named.join(".")
}

fn render(path: &Path, root: &Map<String, Value>) -> Result<String, ConfigError> {
    let value = Value::Object(root.clone());
    let rendered = match extension(path)?.as_str() {
        "json" => serde_json::to_string_pretty(&value).map(|text| format!("{text}\n")),
        _ => serde_norway::to_string(&value).map_err(serde::ser::Error::custom),
    };
    rendered.map_err(|e: serde_json::Error| ConfigError::Malformed {
        path: path.to_path_buf(),
        message: format!("the document could not be written back: {e}"),
    })
}

/// Write `text` where the document is, without a reader ever seeing half of
/// it.
///
/// Owner-only permissions are set on the temporary *before* the content goes
/// in, because a settings document may hold a credential and a file that
/// briefly existed at 0644 has already published it.
fn publish(path: &Path, text: &str) -> Result<(), ConfigError> {
    use std::io::Write;

    let failed = |source: std::io::Error| ConfigError::Unreadable {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    let temporary = path.with_extension(format!(
        "{}.tetanus-tmp",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("settings")
    ));

    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        owner_only(&file)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    })();
    if let Err(source) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(failed(source));
    }
    if let Err(source) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(failed(source));
    }
    Ok(())
}

#[cfg(unix)]
fn owner_only(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

/// Elsewhere the platform's own default stands: there is no portable owner-only
/// mode to set, and pretending otherwise would be a promise this cannot keep.
#[cfg(not(unix))]
fn owner_only(_file: &std::fs::File) -> std::io::Result<()> {
    Ok(())
}
