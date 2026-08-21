//! State that belongs to one scope and goes when the scope does.
//!
//! A session works something out - a resolved workspace root, a cached
//! projection, whether it has already warned about something - and wants to
//! remember it for as long as it is running. Two things must be true of that
//! memory, and neither is true of a plain map on a shared struct.
//!
//! **One scope cannot read another's.** Two sessions in one process, or a child
//! and the parent it forked from, must not see each other's working state:
//! the second would be acting on a fact it did not establish, which is the
//! borrowed-knowledge failure the filesystem observation policy exists to stop,
//! one layer down. Keys are per scope, so there is no spelling of a key that
//! reaches across.
//!
//! **State goes when the scope goes.** A long-lived process that kept a map per
//! session it ever ran would grow without bound, and - worse than the memory -
//! a session id that came back would inherit what a previous run of it believed.
//! Disposal is therefore explicit and it is an [`EffectHandle`]: whoever opened
//! the scope holds the handle, and dropping it takes the state with it, exactly
//! as dropping a plugin's handle takes its registrations.
//!
//! **It is memory, not durability.** Nothing here is written anywhere, and that
//! is the difference from [`crate::storage`]: this holds what a run worked out,
//! and the store holds what should survive it. A caller that wants both puts
//! the durable copy in the store and the working copy here, which is what makes
//! a checkpoint a shortcut rather than an authority.
//!
//! Parity: upstream's Cordis scopes, whose per-scope stores answer the same
//! question. Its scope *keys* and the parent chain that resolves a value from
//! an ancestor have no counterpart, deliberately: an inheriting lookup is how a
//! child ends up acting on a parent's belief, and `docs/parity-updates/` says
//! so.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::effects::EffectHandle;

/// Every scope's state, and the disposal that ends one.
///
/// One instance is shared by whatever needs scoped memory; the scope key keeps
/// them apart inside it.
#[derive(Debug, Default)]
pub struct ScopedStores {
    scopes: Mutex<HashMap<String, HashMap<String, Value>>>,
}

impl ScopedStores {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Open a scope, and hand back the handle that closes it.
    ///
    /// Opening a scope that is already open does not clear it: a caller that
    /// reopens by the same key is continuing, and dropping what was there would
    /// make the second holder's first read depend on whether anybody had opened
    /// it before. Both handles have to go before the state does, which is what
    /// `EffectHandle` composition already means everywhere else in this crate.
    pub fn open(self: &Arc<Self>, scope: impl Into<String>) -> Scope {
        let scope = scope.into();
        self.scopes
            .lock()
            .expect("scoped stores")
            .entry(scope.clone())
            .or_default();
        Scope {
            stores: Arc::clone(self),
            scope,
        }
    }

    /// Read one value, or `None` when this scope never set it.
    pub fn get(&self, scope: &str, key: &str) -> Option<Value> {
        self.scopes
            .lock()
            .expect("scoped stores")
            .get(scope)
            .and_then(|store| store.get(key))
            .cloned()
    }

    /// Write one value, answering what was there.
    ///
    /// A write to a scope nobody opened is kept rather than refused: a caller
    /// that holds a scope key holds it because something gave it one, and
    /// refusing here would turn a lifetime mistake into a silent data loss at
    /// the moment of the write instead of at the moment of the read.
    pub fn set(&self, scope: &str, key: impl Into<String>, value: Value) -> Option<Value> {
        self.scopes
            .lock()
            .expect("scoped stores")
            .entry(scope.to_string())
            .or_default()
            .insert(key.into(), value)
    }

    pub fn remove(&self, scope: &str, key: &str) -> Option<Value> {
        self.scopes
            .lock()
            .expect("scoped stores")
            .get_mut(scope)
            .and_then(|store| store.remove(key))
    }

    /// The keys one scope holds, in key order.
    pub fn keys(&self, scope: &str) -> Vec<String> {
        self.scopes
            .lock()
            .expect("scoped stores")
            .get(scope)
            .map(|store| {
                let mut keys: Vec<String> = store.keys().cloned().collect();
                keys.sort();
                keys
            })
            .unwrap_or_default()
    }

    /// How many scopes are open. For a caller that wants to assert its own
    /// tidiness, and for the case that proves disposal frees.
    pub fn open_scopes(&self) -> usize {
        self.scopes.lock().expect("scoped stores").len()
    }

    fn dispose(&self, scope: &str) {
        self.scopes.lock().expect("scoped stores").remove(scope);
    }
}

/// One scope's view of the store, and the right to end it.
///
/// Holding this is what a session holds. It reads and writes only its own
/// scope, so a caller cannot name another scope by accident: the key is not a
/// parameter of any method here.
pub struct Scope {
    stores: Arc<ScopedStores>,
    scope: String,
}

impl Scope {
    pub fn key(&self) -> &str {
        &self.scope
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.stores.get(&self.scope, key)
    }

    pub fn set(&self, key: impl Into<String>, value: Value) -> Option<Value> {
        self.stores.set(&self.scope, key, value)
    }

    pub fn remove(&self, key: &str) -> Option<Value> {
        self.stores.remove(&self.scope, key)
    }

    pub fn keys(&self) -> Vec<String> {
        self.stores.keys(&self.scope)
    }

    /// Read a value as the type the caller expects, or `None` when it is absent
    /// or is something else.
    ///
    /// Something else reads as absent rather than as an error, deliberately:
    /// this is a cache of what a run worked out, every value in it is
    /// recomputable, and a caller that meets a value it cannot use should
    /// recompute rather than fail. A store whose values *must* be right is
    /// [`crate::storage`], which refuses instead.
    pub fn read<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        serde_json::from_value(self.get(key)?).ok()
    }

    /// Write a value that serializes, answering whether it did.
    ///
    /// A value that cannot be serialized is not written and says so. It is the
    /// one caller mistake this type can catch, and silently dropping it would
    /// produce a read that answers `None` for a value the caller believes it
    /// stored.
    pub fn write<T: serde::Serialize>(&self, key: impl Into<String>, value: &T) -> bool {
        match serde_json::to_value(value) {
            Ok(value) => {
                self.set(key, value);
                true
            }
            Err(_) => false,
        }
    }

    /// The handle that ends this scope.
    ///
    /// Consuming, so a caller cannot hold both the scope and its disposal and
    /// then use the scope after disposing it: what is left after this call is
    /// the handle alone.
    pub fn into_handle(self) -> EffectHandle {
        let stores = Arc::clone(&self.stores);
        let scope = self.scope.clone();
        EffectHandle::new(move || stores.dispose(&scope))
    }
}
