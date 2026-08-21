//! The filesystem a model works through: one service, two backends, the
//! policy that decides what a tool may see and change, and the tools
//! themselves.
//!
//! - [`service`] is the seam: the vocabulary and the [`FileSystem`] trait.
//! - [`error`] is the closed set of reasons an operation did not happen, each
//!   with a machine-routable code and a sentence a model can act on.
//! - [`local`] is the unfenced backend, and the body of the fenced one.
//! - [`sandbox`] is the fenced backend: one workspace, one mode.
//! - [`kernel`] puts the same policy the shell tools run behind in front of any
//!   backend, enforced by Landlock on a worker thread that confines itself.
//! - [`access`] names the modes and chooses the backend one asks for.
//! - [`observation`] is the read-before-write policy: what a session has seen,
//!   and what that lets it change.
//! - [`glob`] is the pattern language the search tool accepts.
//! - [`preset`] names the two permission knobs as one choice a person makes.
//! - [`tools`] registers the model-facing tools over all of it.
//!
//! **The fence is not a security boundary, and [`kernel`] is.** Everything
//! except that module decides which paths this process's own syscalls may
//! name, which is a complete answer while the code doing the naming is ours -
//! `crates/turn/src/fs.rs` draws the distinction at length. [`kernel`] is the
//! other half: it wraps any backend in the kernel boundary `crates/sandbox`
//! prepares, so a path the fence allows and the policy does not is refused by
//! Landlock rather than by cooperation. Isolating untrusted *code* still needs
//! the process-level confinement `crates/exec` applies; the two share one
//! [`tetanus_sandbox::Policy`] so they cannot drift apart.
//!
//! Parity: upstream `packages/fs/*`, restated against the tetanus seams that
//! carry the same decisions.

pub mod access;
pub mod error;
pub mod glob;
pub mod kernel;
pub mod local;
pub mod observation;
pub mod preset;
pub mod sandbox;
pub mod service;
pub mod tools;

pub use access::{backend, FsMode};
pub use error::{FsError, FsErrorCode};
pub use kernel::KernelConfined;
pub use local::LocalFs;
pub use observation::{Observation, ObservedState};
pub use preset::{Preset, DEFAULT_PRESET, PRESETS};
pub use sandbox::SandboxedFs;
pub use service::{
    Deleted, DirEntry, EditOutcome, EditRequest, FileKind, FileSystem, FsInfo, FsTarget, FsVersion,
    WriteIntent, WriteOperation, WriteOutcome,
};
pub use tools::FsTools;
