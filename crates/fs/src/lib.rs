//! The filesystem a model works through: one service, two backends, the
//! policy that decides what a tool may see and change, and the tools
//! themselves.
//!
//! - [`service`] is the seam: the vocabulary and the [`FileSystem`] trait.
//! - [`error`] is the closed set of reasons an operation did not happen, each
//!   with a machine-routable code and a sentence a model can act on.
//! - [`local`] is the unfenced backend, and the body of the fenced one.
//! - [`sandbox`] is the fenced backend: one workspace, one mode.
//! - [`access`] names the modes and chooses the backend one asks for.
//! - [`observation`] is the read-before-write policy: what a session has seen,
//!   and what that lets it change.
//! - [`glob`] is the pattern language the search tool accepts.
//! - [`tools`] registers the model-facing tools over all of it.
//!
//! **Nothing here is a security boundary**, and the distinction is the same one
//! `crates/turn/src/fs.rs` draws at length: these decide which paths this
//! process's own syscalls may name. Isolating untrusted *code* needs a kernel,
//! and no arrangement of these types provides it.
//!
//! Parity: upstream `packages/fs/*`, restated against the tetanus seams that
//! carry the same decisions.

pub mod access;
pub mod error;
pub mod glob;
pub mod local;
pub mod observation;
pub mod sandbox;
pub mod service;
pub mod tools;

pub use access::{backend, FsMode};
pub use error::{FsError, FsErrorCode};
pub use local::LocalFs;
pub use observation::{Observation, ObservedState};
pub use sandbox::SandboxedFs;
pub use service::{
    Deleted, DirEntry, EditOutcome, EditRequest, FileKind, FileSystem, FsInfo, FsTarget, FsVersion,
    WriteIntent, WriteOperation, WriteOutcome,
};
pub use tools::FsTools;
