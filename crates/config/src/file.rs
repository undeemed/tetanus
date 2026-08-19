//! The settings document: the [`Layer::File`] layer, read from disk.
//!
//! Upstream stores one document of per-namespace sections. tetanus resolves
//! flat dotted keys, so a section reads as its keys prefixed by the section
//! name: `log: {level: debug}` is `log.level`.
//!
//! Parity: upstream `packages/settings/settings-file`, pinned by the boot and
//! read cases of its `local.spec.ts`.
//!
//! [`Layer::File`]: crate::Layer::File

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{ConfigError, Document};

/// The document's name under the harness home.
pub const DOCUMENT: &str = "settings.yaml";

/// The extensions a settings document may carry.
pub const EXTENSIONS: [&str; 3] = ["json", "yaml", "yml"];

/// Where the settings document lives when nothing names another path.
pub fn document_path(home: &Path) -> PathBuf {
    home.join(DOCUMENT)
}

/// Read the settings document at `path` as one flat layer document.
///
/// An absent file reads as no settings, so a first run works with nothing on
/// disk. Every other fault is reported: see [`ConfigError`].
pub fn read(path: &Path) -> Result<Document, ConfigError> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !EXTENSIONS.contains(&extension.as_str()) {
        return Err(ConfigError::UnsupportedExtension {
            path: path.to_path_buf(),
            extension,
        });
    }
    if path.is_dir() {
        return Err(ConfigError::IsADirectory {
            path: path.to_path_buf(),
        });
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Document::new()),
        Err(source) => {
            return Err(ConfigError::Unreadable {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    parse(path, &text, &extension)
}

/// Parse document text of a known extension into its flat keys.
///
/// An empty document is no sections, not a parse error: an editor that saves a
/// file it has emptied has said "nothing configured here", and JSON has no way
/// to write that.
fn parse(path: &Path, text: &str, extension: &str) -> Result<Document, ConfigError> {
    if text.trim().is_empty() {
        return Ok(Document::new());
    }
    let malformed = |message: String| ConfigError::Malformed {
        path: path.to_path_buf(),
        message,
    };
    let root: Value = match extension {
        "json" => serde_json::from_str(text).map_err(|e| malformed(e.to_string()))?,
        _ => serde_norway::from_str(text).map_err(|e| malformed(e.to_string()))?,
    };
    match root {
        // A document of only comments parses to nothing, and means nothing.
        Value::Null => Ok(Document::new()),
        Value::Object(sections) => {
            let mut document = Document::new();
            for (key, value) in sections {
                flatten(&key, value, &mut document);
            }
            Ok(document)
        }
        _ => Err(ConfigError::NotAMap {
            path: path.to_path_buf(),
        }),
    }
}

/// Flatten one section into dotted keys.
///
/// A map recurses; anything else is a leaf, arrays included, because a config
/// key holds a list as one value. An empty map therefore contributes no key,
/// which is right: it sets nothing.
fn flatten(prefix: &str, value: Value, document: &mut Document) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                flatten(&format!("{prefix}.{key}"), field, document);
            }
        }
        leaf => {
            document.insert(prefix.to_string(), leaf);
        }
    }
}
