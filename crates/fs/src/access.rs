//! Which paths a tool may see, which it may change, and what a refusal says.
//!
//! Three modes, and the vocabulary is upstream's
//! (`packages/sandbox`, `packages/fs/fs-sandbox`). What differs is where the
//! line falls, and the difference is deliberate.
//!
//! **Upstream fences mutations only; tetanus fences resolution.** Upstream's
//! sandboxed backend passes every read through untouched and judges the two
//! mutations, because its threat model is a model that overwrites the wrong
//! file. tetanus's containment seam ([`tetanus_turn::fs::Workspace`]) judges a
//! path when it is resolved, so a read is fenced too. That is strictly
//! narrower, it costs nothing a coding agent needs, and it means one rule -
//! "the workspace is what exists" - rather than two rules a reader has to keep
//! apart. `docs/parity.md` records the difference.
//!
//! **`danger-full-access` is not a mode of the fenced backend.** A backend
//! configured to confine nothing is a backend that is not confining, and
//! pretending otherwise puts a branch inside the fence whose whole job is to
//! skip the fence - the one branch a mistake there would be silent in. So the
//! mode selects the *backend*: [`backend`] answers a [`crate::LocalFs`] for
//! `danger-full-access` and a [`crate::SandboxedFs`] for the other two.
//!
//! **Neither mode isolates code.** These decide which paths this process's own
//! syscalls may name. A kernel sandbox is a different mechanism for a different
//! threat, and `crates/turn/src/fs.rs` says so at length; nothing here should
//! be read as providing it.

use std::path::Path;
use std::sync::Arc;

use crate::error::FsError;
use crate::local::LocalFs;
use crate::sandbox::SandboxedFs;
use crate::service::FileSystem;

/// How much of the filesystem a session may touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FsMode {
    /// See the workspace, change nothing. Every mutation is refused before it
    /// reaches the disk.
    ReadOnly,
    /// See the workspace and change what is inside it. The default: a
    /// deployment that says nothing gets a fence, not the absence of one.
    #[default]
    WorkspaceWrite,
    /// No fence at all. Named so a deployment that wants it has to write the
    /// word.
    DangerFullAccess,
}

impl FsMode {
    /// The wire and settings spelling, which is upstream's.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    /// Whether this mode permits mutation at all.
    ///
    /// A method rather than a `matches!` at each call site: a mode added later
    /// is one edit here, and forgetting it is a compile error instead of a
    /// gate that silently opens.
    pub const fn may_mutate(self) -> bool {
        match self {
            Self::ReadOnly => false,
            Self::WorkspaceWrite | Self::DangerFullAccess => true,
        }
    }

    /// Read a mode a deployment named.
    ///
    /// An unknown word is refused rather than defaulted, the way
    /// [`tetanus_turn::approval::ApprovalPolicy::parse`] refuses one: a mode is
    /// set by a caller that could have written one of the three, and guessing
    /// which it meant would hide a misconfiguration behind whichever default
    /// was chosen - in one direction a fence nobody asked for, in the other a
    /// fence somebody did.
    pub fn parse(word: &str) -> Result<Self, UnknownMode> {
        match word {
            "read-only" => Ok(Self::ReadOnly),
            "workspace-write" => Ok(Self::WorkspaceWrite),
            "danger-full-access" => Ok(Self::DangerFullAccess),
            other => Err(UnknownMode(other.to_string())),
        }
    }

    /// The sentence a model reads when this mode refuses a mutation.
    ///
    /// Written for the reader that has to decide what to do next: it names the
    /// mode, so the model knows the refusal is a standing rule rather than
    /// something about this path, and it does not suggest retrying - under
    /// `read-only` there is no retry that works.
    pub fn refusal(self, operation: &str) -> String {
        match self {
            Self::ReadOnly => format!(
                "file access is denied under read-only mode: this session may read the workspace \
                 but not {operation} in it. Report what you would have changed instead of \
                 retrying"
            ),
            Self::WorkspaceWrite | Self::DangerFullAccess => format!(
                "file access is denied under {} mode for this {operation}",
                self.as_str()
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "filesystem mode must be \"read-only\", \"workspace-write\" or \"danger-full-access\", not {0:?}"
)]
pub struct UnknownMode(pub String);

/// The backend a mode asks for, rooted at `root`.
///
/// One function so a deployment never composes the pair by hand and never
/// composes it wrong: the mode and the backend cannot disagree if only one
/// place puts them together.
pub fn backend(mode: FsMode, root: impl AsRef<Path>) -> Result<Arc<dyn FileSystem>, FsError> {
    let root = root.as_ref();
    match mode {
        FsMode::DangerFullAccess => Ok(Arc::new(LocalFs::new(root)?)),
        confined => Ok(Arc::new(SandboxedFs::new(root, confined)?)),
    }
}
