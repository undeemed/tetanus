//! Layered config: defaults < file < env < CLI flags, with full
//! provenance (every resolved key remembers which layer set it).
//!
//! Each layer is kept as its own document instead of being folded into one
//! map. The reason is recompose: a settings file a user edits at run time can
//! *drop* a key, and the value under it has to come back. A folded map has
//! nothing to come back to.

use std::collections::BTreeMap;
use std::path::PathBuf;

pub mod file;
pub mod home;
pub mod recompose;
pub mod secret;

/// A fault reading the settings document.
///
/// Every variant names the path, because a harness that reports "could not read
/// settings" without saying which file leaves the user guessing which of the
/// candidate homes it looked in. [`ConfigError::BadValue`] names the key
/// instead: the document was read, and one value in it is not what that key
/// takes.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{}: a settings document is .json, .yaml or .yml, not .{extension}", path.display())]
    UnsupportedExtension { path: PathBuf, extension: String },

    #[error("{}: a settings document, not a directory", path.display())]
    IsADirectory { path: PathBuf },

    #[error("{}: cannot be read: {source}", path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{}: does not parse: {message}", path.display())]
    Malformed { path: PathBuf, message: String },

    #[error("{}: the root must be a map of sections", path.display())]
    NotAMap { path: PathBuf },

    #[error("{key}: must be {expected}, not {found}")]
    BadValue {
        key: String,
        expected: String,
        found: String,
    },
}

/// Where a resolved value came from.
///
/// The variants are declared lowest precedence first, and the derived [`Ord`]
/// is that precedence: of two layers that set the same key, the greater one
/// wins.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Layer {
    Default,
    File,
    Env,
    Flag,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Resolved {
    pub value: serde_json::Value,
    pub layer: Layer,
}

/// One layer's contribution: dotted keys to values, flat.
pub type Document = BTreeMap<String, serde_json::Value>;

#[derive(Default, Debug)]
pub struct Config {
    layers: BTreeMap<Layer, Document>,
    /// Derived from `layers` and kept in step with it by [`Config::resolve`].
    /// Held rather than recomputed per read so [`Config::provenance`] can hand
    /// out references into it.
    resolved: BTreeMap<String, Resolved>,
}

impl Config {
    /// Set one key on one layer. A lower layer never displaces a higher one,
    /// because what a read returns is resolved from the layers, not from the
    /// order the calls arrived in.
    pub fn set(&mut self, key: &str, value: serde_json::Value, layer: Layer) {
        self.layers
            .entry(layer)
            .or_default()
            .insert(key.to_string(), value);
        self.resolve(key);
    }

    /// Replace a whole layer with `document`.
    ///
    /// A key the layer used to set and no longer does falls back to the layer
    /// below it, which is what makes re-reading an edited settings file
    /// correct rather than additive.
    pub fn load(&mut self, layer: Layer, document: Document) {
        let mut touched: Vec<String> = document.keys().cloned().collect();
        if let Some(previous) = self.layers.insert(layer, document) {
            touched.extend(previous.into_keys());
        }
        for key in touched {
            self.resolve(&key);
        }
    }

    pub fn get(&self, key: &str) -> Option<&Resolved> {
        self.resolved.get(key)
    }

    pub fn provenance(&self) -> impl Iterator<Item = (&String, &Resolved)> {
        self.resolved.iter()
    }

    /// Recompute one key from the layers, highest precedence first.
    fn resolve(&mut self, key: &str) {
        let winner = self.layers.iter().rev().find_map(|(layer, document)| {
            document.get(key).map(|value| Resolved {
                value: value.clone(),
                layer: *layer,
            })
        });
        match winner {
            Some(resolved) => {
                self.resolved.insert(key.to_string(), resolved);
            }
            None => {
                self.resolved.remove(key);
            }
        }
    }
}
