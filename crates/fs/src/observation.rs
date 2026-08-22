//! What a session has seen, and what that lets it change.
//!
//! **The rule is one sentence: a tool may not overwrite what it has not
//! read.** Everything here is the machinery for that sentence, and every part
//! of it exists because of a way a model destroys work.
//!
//! - It writes a file it never read, and the file already existed with content
//!   nobody knew about. So an unseen path is written under
//!   [`WriteIntent::CreateIfAbsent`]: it can be created, not clobbered.
//! - It reads a file, thinks for three steps while something else changes it,
//!   and writes back the content it remembers. So a seen path is written under
//!   [`WriteIntent::ReplaceIfVersion`] at the version that was read, and a file
//!   that moved under it is [`FsError::StaleVersion`] instead of a silent
//!   revert.
//! - It edits a file it has not read, guessing at the text to replace. So an
//!   edit of an unseen path is [`FsError::NotObserved`], which says "read it
//!   first" - the one instruction that makes the next attempt work.
//!
//! **State is per owner, and the owner is the session.** Two sessions working
//! in one workspace must not lend each other observations: the second would be
//! writing on the strength of a read the first did, which is exactly the
//! blind write the rule exists to stop. Upstream keys a `WeakMap` on the
//! session object; tetanus keys a map on the session id, and drops a session's
//! state with [`ObservedState::forget`].
//!
//! **An observation is authoritative or it is not recorded.** Only a completed
//! read, stat or write records one, because a guess about what is on the disk
//! is worse than no knowledge at all - it would authorize the write it should
//! have blocked.
//!
//! Parity: upstream `packages/fs/fs-observation-policy`, pinned by its
//! `policy.spec.ts`. Upstream derives the intent through three `fs/*` waterfall
//! events so a deployment can leave the policy out and get unconditional
//! mutation; tetanus makes the policy a value the tool layer holds, and the
//! deployment that wants unconditional mutation composes the tools without one
//! ([`crate::tools::FsTools::unobserved`]).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::FsError;
use crate::service::{FsTarget, FsVersion, WriteIntent};

/// What one owner knows about one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// It was there, at this version.
    Present(FsVersion),
    /// It was confirmed not to be there. Distinct from never having looked:
    /// a confirmed absence authorizes a create, and an unseen path does too,
    /// but only the first makes an *edit* fail with "not found" rather than
    /// "read it first".
    Absent,
}

/// Who observed what.
///
/// One instance is shared by every tool of a deployment; the owner key keeps
/// sessions apart inside it.
#[derive(Debug, Default)]
pub struct ObservedState {
    seen: Mutex<HashMap<String, HashMap<String, Observation>>>,
}

impl ObservedState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an authoritative observation.
    pub fn observe(&self, owner: &str, target: &FsTarget, observation: Observation) {
        self.seen
            .lock()
            .expect("observed state")
            .entry(owner.to_string())
            .or_default()
            .insert(target.key().to_string(), observation);
    }

    /// What this owner last saw at this target, if anything.
    pub fn observation(&self, owner: &str, target: &FsTarget) -> Option<Observation> {
        self.seen
            .lock()
            .expect("observed state")
            .get(owner)
            .and_then(|targets| targets.get(target.key()))
            .cloned()
    }

    /// Drop everything one owner saw.
    ///
    /// Called when a session ends. Without it, a long-lived process
    /// accumulates one entry per file per session it ever ran, and - worse than
    /// the memory - a session id that came back would inherit observations made
    /// before it was resumed, which is the borrowed-knowledge case the whole
    /// module exists to prevent.
    pub fn forget(&self, owner: &str) {
        self.seen.lock().expect("observed state").remove(owner);
    }

    /// The guard the next write of this target runs under.
    ///
    /// Never fails: every state has a safe write. Unseen and confirmed-absent
    /// both authorize a create and nothing more; seen authorizes a replacement
    /// of exactly what was seen.
    pub fn write_intent(&self, owner: &str, target: &FsTarget) -> WriteIntent {
        match self.observation(owner, target) {
            Some(Observation::Present(version)) => WriteIntent::ReplaceIfVersion(version),
            Some(Observation::Absent) | None => WriteIntent::CreateIfAbsent,
        }
    }

    /// The guard the next edit of this target runs under, or why there cannot
    /// be one.
    ///
    /// Unlike a write, an edit of an unseen file has no safe form: the caller
    /// is naming text it believes is in a file it has not looked at.
    pub fn edit_guard(&self, owner: &str, target: &FsTarget) -> Result<FsVersion, FsError> {
        match self.observation(owner, target) {
            Some(Observation::Present(version)) => Ok(version),
            Some(Observation::Absent) => Err(FsError::NotFound {
                path: target.display().to_string(),
            }),
            None => Err(FsError::NotObserved {
                path: target.display().to_string(),
                operation: "edit",
            }),
        }
    }
}
