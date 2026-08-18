//! Core runtime: plugin registry, typed event bus, effect handles.
//! Everything the deepseek-harness does, but with compile-time-checked
//! plugin contracts instead of runtime duck-typing.

pub mod registry;
pub mod events;
pub mod effects;

pub use registry::{Plugin, PluginId, Registry};
pub use events::{Event, EventBus};
pub use effects::EffectHandle;
