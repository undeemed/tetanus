//! Typed service registry: the Rust stand-in for a Cordis context's `ctx.<key>`
//! service slots. A component declares the interface it provides as a [`Service`]
//! definition; boot provides one implementation per definition and every
//! consumer resolves by type, never by importing a concrete implementation.

use std::any::{Any, TypeId};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// A swappable capability. `KEY` is the stable name a human sees (`llm`,
/// `tools`, `sessions`); `Provider` is the interface an implementation satisfies
/// and is usually a trait object.
pub trait Service: 'static {
    const KEY: &'static str;
    type Provider: Send + Sync + ?Sized + 'static;
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("service {0:?} is already provided")]
    Duplicate(&'static str),
    #[error("service {0:?} is not provided")]
    Missing(&'static str),
}

#[derive(Default)]
pub struct Services {
    providers: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    keys: BTreeMap<&'static str, TypeId>,
}

impl Services {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the single implementation of `S`. A second provider for the same
    /// definition is a wiring error, caught here rather than mid-run.
    pub fn provide<S: Service>(&mut self, provider: Arc<S::Provider>) -> Result<(), ServiceError> {
        let key = TypeId::of::<S>();
        if self.providers.contains_key(&key) {
            return Err(ServiceError::Duplicate(S::KEY));
        }
        self.providers.insert(key, Box::new(provider));
        self.keys.insert(S::KEY, key);
        Ok(())
    }

    pub fn get<S: Service>(&self) -> Option<Arc<S::Provider>> {
        self.providers
            .get(&TypeId::of::<S>())?
            .downcast_ref::<Arc<S::Provider>>()
            .cloned()
    }

    pub fn require<S: Service>(&self) -> Result<Arc<S::Provider>, ServiceError> {
        self.get::<S>().ok_or(ServiceError::Missing(S::KEY))
    }

    /// Provided service keys, sorted; `tetanus info` prints these.
    pub fn keys(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.keys.keys().copied()
    }
}
