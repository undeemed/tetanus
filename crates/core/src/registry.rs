use std::collections::BTreeMap;

use crate::context::Context;

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct PluginId(pub String);

impl From<&str> for PluginId {
    fn from(s: &str) -> Self {
        PluginId(s.to_string())
    }
}

/// A plugin is a statically-typed unit of composition. Unlike the JS
/// harness, wiring errors are caught at registration, not mid-run.
pub trait Plugin: Send + Sync + 'static {
    fn id(&self) -> PluginId;
    /// Declared dependencies; the registry topo-sorts and rejects cycles
    /// at insert time (harness parity: "reject cycles at startup").
    fn deps(&self) -> Vec<PluginId> {
        Vec::new()
    }
    /// Mount: provide services and register listeners on the shared context.
    fn start(&self, ctx: &mut Context) -> Result<(), crate::effects::EffectError> {
        let _ = ctx;
        Ok(())
    }
    fn stop(&self) -> Result<(), crate::effects::EffectError> {
        Ok(())
    }
}

#[derive(Default)]
pub struct Registry {
    plugins: BTreeMap<PluginId, Box<dyn Plugin>>,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("duplicate plugin id {0:?}")]
    Duplicate(PluginId),
    #[error("dependency cycle involving {0:?}")]
    Cycle(PluginId),
    #[error("missing dependency {missing:?} required by {by:?}")]
    MissingDep { missing: PluginId, by: PluginId },
    #[error("plugin {id:?} failed to start: {source}")]
    Start {
        id: PluginId,
        #[source]
        source: crate::effects::EffectError,
    },
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, p: Box<dyn Plugin>) -> Result<(), RegistryError> {
        let id = p.id();
        if self.plugins.contains_key(&id) {
            return Err(RegistryError::Duplicate(id));
        }
        self.plugins.insert(id, p);
        Ok(())
    }

    /// Topological start order; errors on cycles/missing deps.
    pub fn start_order(&self) -> Result<Vec<&PluginId>, RegistryError> {
        let mut order = Vec::new();
        let mut state: BTreeMap<&PluginId, u8> = BTreeMap::new(); // 0=unseen 1=visiting 2=done
        fn visit<'a>(
            id: &'a PluginId,
            plugins: &'a BTreeMap<PluginId, Box<dyn Plugin>>,
            state: &mut BTreeMap<&'a PluginId, u8>,
            order: &mut Vec<&'a PluginId>,
        ) -> Result<(), RegistryError> {
            match state.get(id).copied().unwrap_or(0) {
                1 => return Err(RegistryError::Cycle(id.clone())),
                2 => return Ok(()),
                _ => {}
            }
            let (key, plugin) =
                plugins
                    .get_key_value(id)
                    .ok_or_else(|| RegistryError::MissingDep {
                        missing: id.clone(),
                        by: id.clone(),
                    })?;
            state.insert(key, 1);
            for dep in plugin.deps() {
                let dep_key = plugins.get_key_value(&dep).map(|(k, _)| k).ok_or(
                    RegistryError::MissingDep {
                        missing: dep.clone(),
                        by: id.clone(),
                    },
                )?;
                visit(dep_key, plugins, state, order)?;
            }
            state.insert(key, 2);
            order.push(key);
            Ok(())
        }
        for id in self.plugins.keys() {
            visit(id, &self.plugins, &mut state, &mut order)?;
        }
        Ok(order)
    }

    /// Mount every plugin in dependency order, so a consumer always resolves a
    /// service its provider already installed.
    pub fn start_all(&self, ctx: &mut Context) -> Result<Vec<PluginId>, RegistryError> {
        let order: Vec<PluginId> = self.start_order()?.into_iter().cloned().collect();
        for id in &order {
            let plugin = self.plugins.get(id).expect("id came from this registry");
            plugin.start(ctx).map_err(|source| RegistryError::Start {
                id: id.clone(),
                source,
            })?;
        }
        Ok(order)
    }
}
