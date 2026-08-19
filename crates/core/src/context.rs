//! The shared context a boot pass hands to every plugin: the service registry,
//! the event bus, and the effect handles that keep the wiring alive.

use crate::effects::EffectHandle;
use crate::events::EventBus;
use crate::services::Services;

#[derive(Default)]
pub struct Context {
    pub services: Services,
    pub bus: EventBus,
    effects: Vec<EffectHandle>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold a registration for the lifetime of this context. Dropping the
    /// context unwinds every effect in reverse registration order.
    pub fn keep(&mut self, effect: EffectHandle) {
        self.effects.push(effect);
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        while self.effects.pop().is_some() {}
    }
}

impl Context {
    /// Build a context around an existing bus, for components created before
    /// boot that must publish onto the same bus (the session log, notably).
    pub fn with_bus(bus: EventBus) -> Self {
        Self {
            services: Services::new(),
            bus,
            effects: Vec::new(),
        }
    }
}
