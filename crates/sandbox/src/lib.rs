//! Sandboxing: what a run may touch, and the kernel that makes it true.
//!
//! - [`policy`] is the vocabulary - a mode, a workspace root, a network
//!   decision - resolved once at a boundary and handed down whole.
//! - [`landlock`] is the Linux backend: Landlock, the same mechanism upstream's
//!   native helper uses, applied to a child between `fork` and `exec`.
//!
//! **This is a boundary, and `tetanus_turn::fs` is not.** That module fences a
//! *path* this process was asked to open, which is a complete answer while the
//! code doing the opening is ours. This crate answers the other question: a
//! command a model wrote is arbitrary code, and only the kernel can tell it
//! no. The two are complementary and neither replaces the other.
//!
//! **A backend that cannot honour a policy refuses.** There is no mode where
//! asking for confinement and getting none is a success:
//! [`SandboxError::Unavailable`] for a kernel without Landlock,
//! [`SandboxError::Degraded`] for one whose ABI cannot govern what was asked,
//! and a compile-time refusal on a platform with no backend at all
//! ([`unsupported`]). The one way to run unconfined is to say
//! [`Mode::DangerFullAccess`](policy::Mode::DangerFullAccess) out loud.
//!
//! Parity: upstream `packages/sandbox/*` - its policy vocabulary, its local
//! backend's Landlock dialect, its probe, and the Windows ACL family it ships
//! for a platform this cannot honestly serve yet.

pub mod policy;

#[cfg(target_os = "linux")]
pub mod landlock;

#[cfg(not(target_os = "linux"))]
pub mod unsupported;

use std::os::fd::OwnedFd;

pub use policy::{Enforcement, Mode, Network, Policy};

/// What a host can enforce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Support {
    /// Which backend answered.
    pub backend: &'static str,
    /// The Landlock ABI level, where that is the question.
    pub abi: Option<u32>,
    /// Whether this host can deny TCP.
    pub governs_network: bool,
    /// Whether it can govern truncation of an existing file. Before Landlock
    /// ABI 3 a denied-write file could still be emptied, which is a hole worth
    /// naming rather than rounding to "sandboxed".
    pub governs_truncate: bool,
    /// Whether it can govern `ioctl` on a device.
    pub governs_ioctl: bool,
}

/// A policy turned into something the kernel will apply.
///
/// It is built in the parent and applied in the child, so what it carries is a
/// descriptor rather than a plan: everything that could fail has already
/// failed, in the process that can still report it.
#[derive(Debug)]
pub struct Confinement {
    pub backend: &'static str,
    /// How completely the backend governs the policy it was built from.
    pub enforcement: Enforcement,
    /// The kernel object. `None` is only ever an unconfined policy, which the
    /// caller asked for by name.
    pub ruleset: Option<OwnedFd>,
    /// What a denial from *this* backend looks like in a program's own error
    /// text, for a caller that has to tell "the sandbox said no" from "the
    /// command failed". Upstream carries the same per-backend dialect, and for
    /// the same reason: a union across backends claims denials a given backend
    /// never produces.
    pub denial_hints: &'static [&'static str],
}

impl Confinement {
    /// An unconfined run, which is a value rather than an absence so a caller
    /// cannot forget to handle it.
    pub fn none() -> Self {
        Self {
            backend: "none",
            enforcement: Enforcement::Full,
            ruleset: None,
            denial_hints: &[],
        }
    }

    /// Whether this actually confines anything.
    pub fn confines(&self) -> bool {
        self.ruleset.is_some()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// This host has no sandbox to offer. A policy that asked for one is
    /// refused here, at the boundary, rather than by a run that quietly had
    /// none.
    #[error("this host cannot sandbox with {backend}: {why}")]
    Unavailable { backend: &'static str, why: String },
    /// The backend is here but cannot govern everything the policy named, and
    /// the policy did not say partial enforcement was acceptable.
    #[error(
        "{backend} (ABI {abi}) cannot govern {missing}; refuse rather than report a boundary that \
         is not there - a policy may accept partial enforcement explicitly"
    )]
    Degraded {
        backend: &'static str,
        abi: u32,
        missing: String,
    },
    /// The kernel refused an operation this backend needs.
    #[error("{backend} could not {what}: {source}")]
    Kernel {
        backend: &'static str,
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// A root named by the policy cannot be granted.
    #[error("the sandbox cannot grant {path}: {why}")]
    Path { path: String, why: String },
}

/// Prepare `policy` for the platform this build runs on.
///
/// The one entry point every caller uses, so "which backend" is answered in a
/// single place and a platform without one fails the same way everywhere.
pub fn prepare(policy: &Policy) -> Result<Confinement, SandboxError> {
    if !policy.mode().confines() {
        return Ok(Confinement::none());
    }
    #[cfg(target_os = "linux")]
    {
        landlock::prepare(policy)
    }
    #[cfg(not(target_os = "linux"))]
    {
        unsupported::prepare(policy)
    }
}

/// What this host can enforce, for a caller deciding whether to offer
/// confinement at all.
pub fn support() -> Result<Support, SandboxError> {
    #[cfg(target_os = "linux")]
    {
        landlock::support()
    }
    #[cfg(not(target_os = "linux"))]
    {
        unsupported::support()
    }
}
