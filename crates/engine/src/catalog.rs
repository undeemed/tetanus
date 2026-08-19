//! The read-only calls, which answer questions a surface must not answer for
//! itself.
//!
//! Which providers are usable, which tools exist and where a setting came from
//! are facts of the running engine. A surface that worked any of them out again
//! would be a second source of truth, free to disagree with the first.
use std::path::Path;
use std::sync::Arc;

use tetanus_config::{Config, Layer};
use tetanus_protocol::methods::{ConfigDumpResult, ModelCatalogResult, ToolCatalogResult};
use tetanus_protocol::types::{ConfigEntry, ConfigLayer, ProviderDescriptor, ToolDescriptor};
use tetanus_turn::tools::ToolRegistry;

use crate::agent::Providers;

/// The config keys the engine settles for itself, whatever a caller resolved.
pub mod key {
    pub const SESSIONS_ROOT: &str = "sessions.root";
    pub const PROVIDER: &str = "provider.default";
    pub const MODEL: &str = "model.default";
    pub const MAX_STEPS: &str = "agent.max_steps";
}

/// Answers the read-only calls from one place, so `tetanus tools`, a model
/// picker and `tetanus config` all read the engine that is actually running.
pub struct Catalogs {
    providers: Arc<dyn Providers>,
    tools: Arc<ToolRegistry>,
    /// The layered config the caller resolved, kept for its provenance.
    resolved: Arc<Config>,
    /// The engine's own effective values.
    effective: Vec<(&'static str, serde_json::Value)>,
}

impl Catalogs {
    pub fn new(
        providers: Arc<dyn Providers>,
        tools: Arc<ToolRegistry>,
        resolved: Arc<Config>,
        sessions_root: &Path,
        provider: &str,
        model: &str,
        max_steps: u32,
    ) -> Self {
        Self {
            providers,
            tools,
            resolved,
            effective: vec![
                (
                    key::SESSIONS_ROOT,
                    serde_json::json!(sessions_root.display().to_string()),
                ),
                (key::PROVIDER, serde_json::json!(provider)),
                (key::MODEL, serde_json::json!(model)),
                (key::MAX_STEPS, serde_json::json!(max_steps)),
            ],
        }
    }

    /// Every tool a turn on this engine can call, with the schema the model is
    /// offered, so a help surface and the model read one list and a tool
    /// cannot appear in help without being callable.
    pub fn tools(&self) -> ToolCatalogResult {
        let mut tools: Vec<ToolDescriptor> = self
            .tools
            .schemas()
            .into_iter()
            .map(|schema| ToolDescriptor {
                name: schema.name,
                description: schema.description,
                parameters: schema.parameters,
            })
            .collect();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        ToolCatalogResult { tools }
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

    /// The resolved configuration, with provenance.
    ///
    /// For a key the engine settles, the value is the one the engine will
    /// actually use, and the layer is where the caller resolved it from, or
    /// `Default` when the caller never named it. Every other key the caller
    /// resolved is reported as the caller has it, so a config surface shows
    /// one list rather than two that have to be reconciled.
    pub fn dump(&self) -> ConfigDumpResult {
        let mut entries: Vec<ConfigEntry> = self
            .effective
            .iter()
            .map(|(key, value)| ConfigEntry {
                key: (*key).to_string(),
                value: value.clone(),
                layer: self
                    .resolved
                    .get(key)
                    .map_or(ConfigLayer::Default, |entry| layer(entry.layer)),
            })
            .collect();

        for (key, entry) in self.resolved.provenance() {
            if self.effective.iter().any(|(settled, _)| settled == key) {
                continue;
            }
            entries.push(ConfigEntry {
                key: key.clone(),
                value: entry.value.clone(),
                layer: layer(entry.layer),
            });
        }
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        ConfigDumpResult { entries }
    }
}

fn layer(layer: Layer) -> ConfigLayer {
    match layer {
        Layer::Default => ConfigLayer::Default,
        Layer::File => ConfigLayer::File,
        Layer::Env => ConfigLayer::Env,
        Layer::Flag => ConfigLayer::Flag,
    }
}
