//! The watcher wired to the re-read: what actually happens when a user edits
//! `settings.yaml` while the harness is running.
//!
//! Two halves existed and nothing joined them. [`crate::watch::Watcher`] knows
//! when the document has changed and stopped changing;
//! [`crate::recompose::recompose`] knows what a re-read does to a running
//! configuration. This is the piece that turns one into the other, and it is
//! deliberately small - the interesting decisions are already made in those two
//! modules, and a joining layer that added rules of its own would be a third
//! place to look when a re-read does something surprising.
//!
//! **It is a step, not a thread.** [`Reload::tick`] takes one observation and
//! answers what changed. The caller owns the clock and the loop, exactly as
//! `Watcher` does and for the same reason: a decision that is a function of
//! observations can be driven precisely by a test and driven from whatever
//! runtime a deployment already has, and neither has to be a background thread
//! this crate spawns and nobody can see into.
//!
//! **A bad edit is reported and the configuration stands.** `recompose`
//! guarantees the second half; what this adds is that a fault does not stop the
//! watching. One typo would otherwise be permanent until a restart, and the
//! user's next action - fixing it - would appear to do nothing.
//!
//! **The schema is checked on every re-read, not only at boot.** A document
//! that gains a scalar where a section belongs is refused at run time the way
//! it would be refused at startup, or a harness that booted clean could be
//! edited into a state it would have refused to start in.
//!
//! Parity: upstream `packages/settings/settings-file`, whose watcher
//! republishes resolved settings on every settled edit.

use std::path::{Path, PathBuf};

use crate::recompose::{recompose, Recomposed};
use crate::schema::Schema;
use crate::watch::{Stamp, Watcher};
use crate::{Config, ConfigError, Layer};

/// What one tick of the loop found.
#[derive(Debug)]
pub enum Change {
    /// Nothing has settled since the last tick. The overwhelmingly common
    /// answer, and it costs one `stat`.
    None,
    /// The document settled into a new state and the configuration was
    /// re-read. Empty `changed` means it settled back to what it already said,
    /// which is what an editor saving an unmodified buffer produces.
    Applied(Recomposed),
    /// The document settled into a state that could not be used. The running
    /// configuration is exactly as it was, and the watcher goes on watching.
    Refused(ConfigError),
}

impl Change {
    /// Whether this tick changed what any reader would resolve.
    pub fn is_effective(&self) -> bool {
        matches!(self, Self::Applied(recomposed) if !recomposed.is_empty())
    }
}

/// A document, a watcher over it, and the schema its contents must satisfy.
pub struct Reload {
    watcher: Watcher,
    schema: Schema,
}

impl Reload {
    /// Watch `path`, checking what it holds against `schema`.
    ///
    /// The current state is the baseline, so starting a reload against an
    /// existing document does not immediately report it: a deployment that
    /// wants the document read at startup reads it at startup, which is
    /// `crate::file::read`, and conflating the two would make every boot look
    /// like an edit.
    pub fn new(path: impl Into<PathBuf>, schema: Schema) -> Self {
        Self {
            watcher: Watcher::new(path),
            schema,
        }
    }

    /// Watch without a schema, for a deployment that declares nothing.
    pub fn unchecked(path: impl Into<PathBuf>) -> Self {
        Self::new(path, Schema::new())
    }

    /// How many identical observations make a change settled;
    /// [`crate::watch::Watcher::settle_after`] says when to raise it.
    pub fn settle_after(mut self, polls: u32) -> Self {
        self.watcher = self.watcher.settle_after(polls);
        self
    }

    pub fn path(&self) -> &Path {
        self.watcher.path()
    }

    /// Take one observation and, if the document has settled into a new state,
    /// re-read it into `config`.
    pub fn tick(&mut self, config: &mut Config) -> Change {
        match self.watcher.poll() {
            Some(_) => self.apply(config),
            None => Change::None,
        }
    }

    /// The same step over an observation the caller made, so a case can drive
    /// a sequence of states without waiting on a filesystem's timestamp
    /// granularity.
    pub fn observe(&mut self, seen: Stamp, config: &mut Config) -> Change {
        match self.watcher.observe(seen) {
            Some(_) => self.apply(config),
            None => Change::None,
        }
    }

    /// Re-read the document into `config`, under the schema.
    ///
    /// The schema is applied to what the file holds *before* the layer is
    /// replaced, so a refused document leaves the running configuration
    /// untouched - the same promise `recompose` makes about a document that
    /// does not parse, extended to one that parses into something no reader
    /// would accept.
    fn apply(&self, config: &mut Config) -> Change {
        if !self.schema.is_empty() {
            match crate::file::read(self.path()).and_then(|document| self.schema.accept(document)) {
                Ok(_) => {}
                Err(refused) => return Change::Refused(refused),
            }
        }
        match recompose(config, self.path()) {
            Ok(recomposed) => Change::Applied(recomposed),
            Err(refused) => Change::Refused(refused),
        }
    }
}

/// Read the document once, now, into the file layer - the startup half a
/// [`Reload`] deliberately does not do.
///
/// Here rather than at the call site because the schema check belongs with the
/// read, and a caller that did the two in the other order would have loaded a
/// document before finding out it was refused.
pub fn load(config: &mut Config, path: &Path, schema: &Schema) -> Result<(), ConfigError> {
    let document = schema.accept(crate::file::read(path)?)?;
    config.load(Layer::File, document);
    Ok(())
}
