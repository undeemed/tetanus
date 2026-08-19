//! Recompose: re-reading the settings document while the harness runs.
//!
//! Upstream watches the document and republishes the resolved settings on
//! every edit. tetanus has no watcher yet, so what ports is the fold itself:
//! what a re-read does to a running configuration, and what a bad one must not
//! do to it. The watcher, its debounce, and the write path stay rows in
//! `docs/parity.md`.
//!
//! Parity: upstream `packages/settings/settings-file`, the runtime half of its
//! `watcher.spec.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use crate::{Config, ConfigError, Layer};

/// What one recompose changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recomposed {
    /// Every key whose resolved value or owning layer is no longer what it
    /// was, in key order. A key the document dropped is named here too: what
    /// it resolves to now is a different answer, even when the answer is
    /// nothing at all.
    pub changed: Vec<String>,
}

impl Recomposed {
    /// Whether the re-read left every resolved key exactly as it was. An
    /// editor that saves a file it did not change is the common case, and a
    /// caller that republishes on every save republishes nothing new.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }
}

/// Re-read the settings document at `path` and replace [`Layer::File`] with
/// what it holds now.
///
/// A document that is not there reads as no settings, so a deleted file hands
/// every key it used to set back to the layer under it. Every other fault
/// leaves the running configuration exactly as it was and is returned: a bad
/// edit at run time must not empty a harness that is working.
pub fn recompose(config: &mut Config, path: &Path) -> Result<Recomposed, ConfigError> {
    let document = crate::file::read(path)?;
    let before = snapshot(config);
    config.load(Layer::File, document);
    let after = snapshot(config);
    Ok(Recomposed {
        changed: differences(&before, &after),
    })
}

type Snapshot = BTreeMap<String, (Value, Layer)>;

fn snapshot(config: &Config) -> Snapshot {
    config
        .provenance()
        .map(|(key, resolved)| (key.clone(), (resolved.value.clone(), resolved.layer)))
        .collect()
}

fn differences(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|key| before.get(*key) != after.get(*key))
        .cloned()
        .collect()
}
