//! Every platform that has no backend here yet, refusing in the one way that
//! is honest.
//!
//! **Windows.** Upstream ships a real backend
//! (`packages/sandbox/sandbox-windows-acl`): it mints a per-workspace SID,
//! grants it on the workspace with an inheritable ACE, builds a restricted
//! token, and spawns the child under it - about fifteen hundred lines across
//! an FFI layer, a token builder, an ACL grant, a path-boundary check and a
//! runner. Writing a Rust equivalent is a real slice of work, and every part
//! of it is untestable from this lane: there is no Windows host in this
//! workspace's CI, so the only thing that could be asserted is that the code
//! compiles. A sandbox nobody has ever seen deny anything is not a sandbox,
//! and a backend that returned success while doing nothing would be worse than
//! the refusal below, because a deployment would believe it.
//!
//! So the trait shape stays - [`crate::prepare`] is the same call on every
//! platform, and the Windows backend is a module to fill in rather than a
//! design to revisit - and asking for confinement here fails loudly, naming
//! what would have to be built. `docs/parity.md` carries it as a named
//! follow-up with the same reasoning.
//!
//! **macOS.** Upstream uses Seatbelt (`sandbox-exec`). The same argument
//! applies: no host to prove it on from here.

use crate::policy::Policy;
use crate::{Confinement, SandboxError, Support};

/// The name of the backend this platform would need.
pub const WOULD_NEED: &str = if cfg!(windows) {
    "a Windows restricted-token and ACL backend (upstream's sandbox-windows-acl)"
} else if cfg!(target_os = "macos") {
    "a Seatbelt backend (upstream's sandbox-local darwin dialect)"
} else {
    "a kernel confinement backend for this platform"
};

/// Refuse, naming what is missing.
///
/// A caller that genuinely wants no confinement says so in the policy
/// ([`crate::Mode::DangerFullAccess`]) and never reaches here.
pub fn prepare(policy: &Policy) -> Result<Confinement, SandboxError> {
    Err(SandboxError::Unavailable {
        backend: "none",
        why: format!(
            "{} is not built yet, so `{}` cannot be enforced on this platform; a policy that must \
             run here has to say `danger-full-access` deliberately rather than be given it \
             silently",
            WOULD_NEED,
            policy.mode()
        ),
    })
}

/// Report the absence rather than a capability nobody implemented.
pub fn support() -> Result<Support, SandboxError> {
    Err(SandboxError::Unavailable {
        backend: "none",
        why: format!("{WOULD_NEED} is not built yet"),
    })
}
