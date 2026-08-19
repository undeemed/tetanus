//! The tetanus engine<->presentation contract.
//!
//! This crate is the machine-readable half of `docs/interface-contract.md`.
//! The document is the spec a human reads; these types are what both lanes
//! compile against. They must agree, and the doc's changelog records every
//! change to either.
//!
//! Three rules make the boundary hold:
//!
//! - **The engine lane owns this crate.** A presentation surface consumes it
//!   and never edits it. A change lands as its own pull request touching the
//!   document and these types together.
//! - **Nothing here depends on an engine crate.** The engine converts its
//!   internal types into these wire shapes, so refactoring the engine is not a
//!   breaking change.
//! - **Presentation is not in the contract.** Colour, layout, progress
//!   rendering and help wording are the other lane's. This crate carries the
//!   facts a surface renders, never the rendering.

pub mod methods;
pub mod rpc;
pub mod types;

pub use methods::{capability, method, push, Engine};
pub use rpc::{ErrorCode, Id, Message, Notification, Request, Response, RpcError};

/// The contract version this build serves, `major.minor`.
///
/// A major bump means an existing call, field, enum variant or error code
/// changed meaning or disappeared. A minor bump means only additions. A server
/// refuses a client whose major differs, and accepts any minor.
pub const PROTOCOL_VERSION: &str = "1.0";

/// The major component of a `major.minor` string, or `None` when it is not one.
pub fn protocol_major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

/// Whether this build can serve a peer built against `version`.
pub fn is_compatible(version: &str) -> bool {
    match (protocol_major(version), protocol_major(PROTOCOL_VERSION)) {
        (Some(theirs), Some(ours)) => theirs == ours,
        _ => false,
    }
}
