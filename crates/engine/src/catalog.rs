//! The read-only calls, which answer questions a surface must not answer for
//! itself.
//!
//! Which providers are usable, which tools exist and where a setting came from
//! are facts of the running engine. A surface that worked any of them out again
//! would be a second source of truth, free to disagree with the first.

use std::sync::Arc;

use tetanus_protocol::methods::ModelCatalogResult;
use tetanus_protocol::types::ProviderDescriptor;

use crate::agent::Providers;

/// Answers the read-only calls from one place, so every surface reads the
/// engine that is actually running.
pub struct Catalogs {
    providers: Arc<dyn Providers>,
}

impl Catalogs {
    pub fn new(providers: Arc<dyn Providers>) -> Self {
        Self { providers }
    }

    /// Every provider this build can route to, and whether it could run right
    /// now. `available: false` is how a picker greys an entry out instead of
    /// offering it and meeting `MissingCredential` on the first turn.
    pub fn models(&self) -> ModelCatalogResult {
        ModelCatalogResult {
            providers: self
                .providers
                .all()
                .into_iter()
                .map(|adapter| ProviderDescriptor {
                    provider: adapter.provider().to_string(),
                    models: adapter.models(),
                    credential_env: adapter.credential_env().map(str::to_string),
                    // An adapter that needs no credential is always usable.
                    available: adapter
                        .credential_env()
                        .is_none_or(|env| !std::env::var(env).unwrap_or_default().is_empty()),
                })
                .collect(),
        }
    }
}
