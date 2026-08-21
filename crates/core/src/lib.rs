//! Core runtime: plugin registry, typed service registry, typed event bus,
//! RAII effect handles, and a small durable key-value store for what a run
//! works out rather than what it observed. Everything the deepseek-harness does, but with
//! compile-time-checked contracts instead of runtime duck-typing.

pub mod context;
pub mod effects;
pub mod events;
pub mod jobs;
pub mod registry;
pub mod schedule;
pub mod scoped;
pub mod services;
pub mod spill;
pub mod storage;

pub use context::Context;
pub use effects::{EffectError, EffectHandle, EffectScope};
pub use events::{BoxFuture, DispatchMode, Event, EventBus, Next, Terminal};
pub use registry::{Plugin, PluginId, Registry, RegistryError};
pub use scoped::{Scope, ScopedStores};
pub use services::{Service, ServiceError, Services};
