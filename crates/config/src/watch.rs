//! Noticing that the settings document changed.
//!
//! [`recompose`](crate::recompose::recompose) has been able to re-read a
//! document since it was written, and nothing ever called it: a user who
//! edited `settings.yaml` while the harness ran saw no effect until a restart.
//! This is the half that notices.
//!
//! **It polls, and that is upstream's mechanism too.** Upstream watches with
//! chokidar under a `stabilityThreshold`, which is a poll: it waits for the
//! size and time to stop moving before reporting a change, because an editor
//! writing a file produces several events and only the last one is worth
//! reading. Polling directly needs no dependency, behaves the same on every
//! platform, and makes the settle rule explicit rather than a library's
//! option.
//!
//! **A change is only reported once the file has stopped moving.** An editor
//! that truncates and rewrites is momentarily an empty document, and a reader
//! that fired on the first event would parse an empty file, drop every key,
//! and hand a running harness the defaults. Waiting for one quiet interval
//! costs a moment and removes that entirely.
//!
//! **A fault is not a change.** A document that is mid-write, or briefly
//! unreadable, must leave the running configuration alone;
//! [`recompose`](crate::recompose::recompose) already guarantees that, and
//! what this adds is that the watcher keeps watching afterwards. A watcher
//! that stopped on the first bad edit would make one typo permanent until a
//! restart, which is the failure it exists to prevent.
//!
//! Parity: upstream `packages/settings/settings-file`, the watcher half of its
//! `watcher.spec.ts`. Its dispose-quiesce and write-path cases belong to a
//! service that also writes; this only reads.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// What the filesystem says about the document right now.
///
/// Absence is a state and not an error: a deleted document is a real edit,
/// and one that hands every key it set back to the layer beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub present: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl Stamp {
    /// Read the current stamp. Any failure to stat reads as absent, because
    /// the two are the same thing to a watcher: the document it can see is not
    /// there.
    pub fn of(path: &Path) -> Self {
        match std::fs::metadata(path) {
            Ok(meta) => Self {
                present: true,
                len: meta.len(),
                modified: meta.modified().ok(),
            },
            Err(_) => Self {
                present: false,
                len: 0,
                modified: None,
            },
        }
    }
}

/// Watches one path and says when it has settled into a new state.
///
/// Deliberately not a thread. The decision - has this changed, and has it
/// stopped changing - is what is worth testing, and it is a function of two
/// observations. [`Watcher::poll`] takes one observation; the caller owns the
/// clock and the loop, so a test drives it exactly and a deployment drives it
/// from wherever its runtime already is.
#[derive(Debug)]
pub struct Watcher {
    path: PathBuf,
    /// The stamp the last reported state had.
    settled: Stamp,
    /// A stamp seen since, not yet reported, and how many polls it has been
    /// stable for.
    pending: Option<(Stamp, u32)>,
    /// How many consecutive identical polls make a change settled.
    quiet_polls: u32,
}

impl Watcher {
    /// Start watching `path`, taking its current state as the baseline.
    ///
    /// The baseline is what is there now, so a watcher started against an
    /// existing document does not immediately report it as a change. A
    /// deployment that wants the document read at startup reads it at startup;
    /// that is not this.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let settled = Stamp::of(&path);
        Self {
            path,
            settled,
            pending: None,
            quiet_polls: 1,
        }
    }

    /// How many identical observations are needed before a change is
    /// reported.
    ///
    /// One is the default and is enough when polls are far apart relative to a
    /// write. A caller polling very fast should ask for more, because the
    /// window in which a half-written file looks stable grows as the interval
    /// shrinks.
    pub fn settle_after(mut self, polls: u32) -> Self {
        self.quiet_polls = polls.max(1);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Take one observation.
    ///
    /// Answers `Some` exactly once per settled change: when the document has
    /// moved and then held still. A caller that polls a document nobody is
    /// editing gets `None` for ever and does no work.
    pub fn poll(&mut self) -> Option<Stamp> {
        self.observe(Stamp::of(&self.path))
    }

    /// The same decision, over an observation the caller made.
    ///
    /// This is what the cases drive: a watcher's rule is about a sequence of
    /// observations, and reading real files to produce that sequence would be
    /// testing the filesystem's timestamp granularity instead.
    pub fn observe(&mut self, seen: Stamp) -> Option<Stamp> {
        if seen == self.settled {
            // Back to where it started counts as nothing happening, which is
            // what an editor that saves an unchanged buffer produces.
            self.pending = None;
            return None;
        }
        match self.pending {
            Some((pending, held)) if pending == seen => {
                let held = held + 1;
                if held >= self.quiet_polls {
                    self.settled = seen;
                    self.pending = None;
                    return Some(seen);
                }
                self.pending = Some((seen, held));
            }
            // Either the first sighting of this state, or a different one from
            // the state that was pending: the file is still moving, so the
            // count starts again.
            _ => {
                self.pending = Some((seen, 1));
                if self.quiet_polls == 1 {
                    self.settled = seen;
                    self.pending = None;
                    return Some(seen);
                }
            }
        }
        None
    }
}

/// A sensible gap between polls: often enough that an edit is picked up while
/// the person who made it is still looking at the screen, rare enough that the
/// cost is nothing.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);
