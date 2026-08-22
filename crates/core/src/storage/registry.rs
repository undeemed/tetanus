//! The named registry that lets a deployment mount more than one store and a
//! consumer ask for the one it was configured with.
//!
//! It exists now because there are two backends. A registry over one backend
//! is a map with one entry and a lookup that cannot fail usefully; over two it
//! is the thing that keeps the choice out of the consumer, which is the whole
//! point of having a seam.
//!
//! **Mounted side by side, not chosen globally.** Several backends stay
//! registered at once and which one serves which consumer is that consumer's
//! configuration. A hub-wide "current backend" would make the choice
//! un-scopeable: two components with different needs could not have different
//! answers.
//!
//! **Registration is an effect.** It returns an [`EffectHandle`] like every
//! other registration in this crate, and dropping the handle unregisters the
//! name. Dropping it does *not* close the store: the owner of the store closes
//! it, after unregistering, because a registry that closed what it borrowed
//! would break the second holder of the same store.
//!
//! Parity: upstream `packages/storage/src/registry.ts`, including the rule
//! that a stale disposer must not remove a successor registered under the same
//! name.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use crate::effects::EffectHandle;

use super::{KvStore, StorageError};

/// A store as a registry holds it: shared, and serialized for writing.
///
/// `Mutex` and not a lock-free cell, because [`KvStore::put`] takes `&mut
/// self` and two callers writing the same medium at once is exactly what a
/// store must not permit. The lock is the medium's, so two *different* stores
/// never wait on each other.
pub type SharedStore = Arc<Mutex<dyn KvStore + Send>>;

/// Named stores, mounted together.
#[derive(Default, Clone)]
pub struct StorageRegistry {
    stores: Arc<Mutex<BTreeMap<String, SharedStore>>>,
}

impl StorageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount a store under a name.
    ///
    /// A name that is already mounted is refused rather than replaced: a
    /// deployment that registers two stores under one name has a configuration
    /// mistake, and silently keeping the second would give half its consumers
    /// the wrong medium with nothing to see in a log.
    pub fn register(
        &self,
        name: impl Into<String>,
        store: SharedStore,
    ) -> Result<EffectHandle, StorageError> {
        let name = name.into();
        super::valid_name("store", &name)?;
        let mut stores = self.stores.lock().expect("storage registry");
        if stores.contains_key(&name) {
            return Err(StorageError::DuplicateStore { name });
        }
        stores.insert(name.clone(), Arc::clone(&store));
        drop(stores);

        let held: Weak<Mutex<BTreeMap<String, SharedStore>>> = Arc::downgrade(&self.stores);
        Some(store)
            .map(|registered| {
                EffectHandle::new(move || {
                    if let Some(stores) = held.upgrade() {
                        let mut stores = stores.lock().expect("storage registry");
                        // Only this registration's own contribution. After a
                        // dispose and a re-register under the same name, a stale
                        // handle firing again must not remove the successor.
                        if stores
                            .get(&name)
                            .is_some_and(|current| Arc::ptr_eq(current, &registered))
                        {
                            stores.remove(&name);
                        }
                    }
                })
            })
            .ok_or_else(|| unreachable!())
    }

    /// The store mounted under `name`.
    ///
    /// The error names what *is* mounted, because the commonest cause is a
    /// deployment that spelled the name differently in two places, and a
    /// message that lists the alternatives ends that hunt immediately.
    pub fn get(&self, name: &str) -> Result<SharedStore, StorageError> {
        let stores = self.stores.lock().expect("storage registry");
        stores
            .get(name)
            .map(Arc::clone)
            .ok_or_else(|| StorageError::UnknownStore {
                name: name.to_string(),
                mounted: stores.keys().cloned().collect(),
            })
    }

    /// Every mounted name, in name order.
    pub fn names(&self) -> Vec<String> {
        self.stores
            .lock()
            .expect("storage registry")
            .keys()
            .cloned()
            .collect()
    }
}
