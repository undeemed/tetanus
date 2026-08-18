//! Phase ① turn engine, mirroring the documented upstream flow:
//! turn/start → claim input → assemble prompt → agent/pre-step →
//! step (llm stream, tool pipeline) → step/end → agent/turn-stopping.

pub mod event;
pub mod engine;
pub mod llm;
pub mod tools;
