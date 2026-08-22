//! Where a projection's fold is kept between runs, so a cold read does not
//! replay the whole journal.
//!
//! [`Projections::checkpoint`](crate::projection::Projections::checkpoint) and
//! [`restore`](crate::projection::Projections::restore) have existed since the
//! projection seam landed, and nothing has ever written one down: every reader
//! folded from seq zero on every open, which is the cost the checkpoint was
//! built to avoid. This is the durable half, over the key-value store.
//!
//! **A row is a shortcut and never an authority.** The log is the authority.
//! A stored row may be stale - its `seq` says how stale, and the tail is
//! folded onto it - but it can never be *wrong*, because
//! [`restore`](crate::projection::Projections::restore) refuses a row whose
//! unit version differs and a row claiming events the log in hand does not
//! have. That refusal is what makes writing these down safe at all.
//!
//! **Every path is fail-soft.** A cache that cannot be read is an empty cache,
//! a cache that cannot be written is a longer replay next time, and a row that
//! does not parse is discarded rather than repaired. None of it is an error a
//! caller has to handle, because the correct behaviour without a cache is the
//! behaviour tetanus had before there was one - and a session that refused to
//! open because its *cache* was corrupt would be a session lost to an
//! optimisation.
//!
//! **One row per session.** A row keyed by session id, in one declared table,
//! so two sessions never share a fold and a store shared with other components
//! keeps its own tables (`tetanus_core::storage`).
//!
//! Parity: upstream `packages/session/session-projection-cache`, including its
//! own summary of the arrangement - "a fold shortcut, never an authority: a row
//! is possibly stale but never wrong, so every write path is fail-soft and a
//! `ver` mismatch discards the row instead of migrating it".

use std::collections::BTreeMap;
use std::sync::Arc;

use tetanus_core::storage::{SharedStore, StorageError};

use crate::projection::{Checkpoint, Projections};
use crate::SessionEvent;

/// The table a cache declares when it opens its store.
pub const TABLE: &str = "session.projections";

/// A durable home for projection checkpoints.
pub struct ProjectionCache {
    store: SharedStore,
}

impl ProjectionCache {
    /// Use `store` as the cache. The caller opened it, declared [`TABLE`], and
    /// keeps it: a cache does not own a medium, because the same store holds
    /// other components' tables.
    pub fn new(store: SharedStore) -> Self {
        Self { store }
    }

    /// What was folded for `session`, or nothing at all.
    ///
    /// Nothing at all covers every way of not having a usable row - no row, a
    /// store that would not read, a row that does not parse as checkpoints -
    /// because the answer to each is identical: fold from the beginning.
    pub fn load(&self, session: &str) -> BTreeMap<String, Checkpoint> {
        let stored = self
            .store
            .lock()
            .expect("the cache store")
            .get(TABLE, session);
        match stored {
            Ok(Some(value)) => match serde_json::from_value(value) {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::debug!(%session, %error, "a projection cache row does not parse; refolding");
                    BTreeMap::new()
                }
            },
            Ok(None) => BTreeMap::new(),
            Err(error) => {
                tracing::debug!(%session, %error, "the projection cache could not be read; refolding");
                BTreeMap::new()
            }
        }
    }

    /// Write what has been folded so far. Answers whether it was stored.
    ///
    /// The answer is a `bool` and not a `Result` on purpose: there is nothing
    /// for a caller to do about a failure except carry on, and a `Result` here
    /// would invite a `?` that turns a slow next open into a failed turn.
    pub fn save(&self, session: &str, rows: &BTreeMap<String, Checkpoint>) -> bool {
        let value = match serde_json::to_value(rows) {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(%session, %error, "projection checkpoints do not serialize");
                return false;
            }
        };
        match self
            .store
            .lock()
            .expect("the cache store")
            .put(TABLE, session, value)
        {
            Ok(_) => true,
            Err(error) => {
                tracing::debug!(%session, %error, "the projection cache could not be written");
                false
            }
        }
    }

    /// Forget one session's fold, when the session itself is gone. Answers
    /// whether there was a row to forget.
    ///
    /// A cache row for a journal that no longer exists is not dangerous - the
    /// next reader has no log to fold it onto - but it is a row nobody will
    /// ever read again, and a store that only grows is a store somebody
    /// eventually has to explain.
    ///
    /// "There was no row" and "the store refused" both answer `false`, which
    /// is the same fail-soft rule the rest of this type keeps: neither is
    /// something a caller deleting a session can act on.
    pub fn forget(&self, session: &str) -> bool {
        self.store
            .lock()
            .expect("the cache store")
            .remove(TABLE, session)
            .map(|previous| previous.is_some())
            .unwrap_or(false)
    }

    /// Warm `projections` from the cache and fold the tail of `events` onto
    /// it. Answers the keys whose value changed, as `drive` does.
    ///
    /// The one call a reader of a cold session needs: which rows were usable
    /// is [`Projections::restore`]'s decision, and it is made per unit, so a
    /// cache holding one stale unit still saves the fold for the others.
    pub fn warm(
        &self,
        session: &str,
        projections: &Arc<Projections>,
        events: &[SessionEvent],
    ) -> Vec<String> {
        projections.restore(&self.load(session), events)
    }
}

/// Declare the cache's table when opening a store for one.
///
/// A caller that opens its own store still has to declare the table, and this
/// is the name to declare - published so a deployment sharing one store
/// between components does not have to copy a string literal.
pub fn declare() -> &'static str {
    TABLE
}

/// The error type a caller sees if it opens the store itself.
///
/// Re-exported so a composition wiring a cache does not have to name
/// `tetanus_core` for one type.
pub type CacheStorageError = StorageError;
