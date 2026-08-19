//! The shared context a boot pass hands to every plugin: the service registry,
//! the event bus, and the effect handles that keep the wiring alive.

use crate::effects::{EffectHandle, EffectScope};
use crate::events::EventBus;
use crate::services::Services;

#[derive(Default)]
pub struct Context {
    pub services: Services,
    pub bus: EventBus,
    effects: EffectScope,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold a registration for the lifetime of this context. Dropping the
    /// context unwinds every effect, newest first: the scope owns that order,
    /// so the context does not restate it.
    pub fn keep(&mut self, effect: EffectHandle) {
        self.effects.keep(effect);
    }
}

impl Context {
    /// Build a context around an existing bus, for components created before
    /// boot that must publish onto the same bus (the session log, notably).
    pub fn with_bus(bus: EventBus) -> Self {
        Self {
            services: Services::new(),
            bus,
            effects: EffectScope::new(),
        }
    }
}
