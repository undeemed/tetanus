//! Core runtime: plugin registry, typed service registry, typed event bus,
//! and RAII effect handles. Everything the deepseek-harness does, but with
//! compile-time-checked contracts instead of runtime duck-typing.

pub mod context;
pub mod effects;
pub mod events;
pub mod registry;
pub mod services;

pub use context::Context;
pub use effects::{EffectError, EffectHandle};
pub use events::{BoxFuture, DispatchMode, Event, EventBus, Next, Terminal};
pub use registry::{Plugin, PluginId, Registry, RegistryError};
pub use services::{Service, ServiceError, Services};
