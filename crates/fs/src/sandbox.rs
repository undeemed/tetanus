//! The fenced backend: the same syscalls, judged against a workspace first.
//!
//! It is [`crate::LocalFs`] with two rules in front of it.
//!
//! **Every path is resolved through the workspace fence.** A path that lands
//! outside is refused before any syscall touches it, and the refusal names the
//! root - so a model that guessed at an absolute path learns where it is
//! allowed to work rather than just that it failed. The fence itself is
//! [`tetanus_turn::fs::Workspace`], which canonicalizes each component as it
//! walks, judges a symlink by where it goes, and judges a path that does not
//! exist yet by where it would land.
//!
//! **Every mutation is judged against the mode.** Under `read-only` a write,
//! an edit and a delete are refused with a sentence saying so; under
//! `workspace-write` they proceed, already known to be inside the fence
//! because that is what resolution guaranteed. The mode is checked at the last
//! moment before the disk is touched rather than at resolution, because a
//! read-only session must still be able to *resolve* and read paths.
//!
//! **A listing never names a path this backend would refuse to open.** A child
//! that is a symlink out of the workspace is left out of the listing and out of
//! a glob's answers, rather than being offered and then denied: inviting a
//! model to try something that cannot work wastes a turn and reads to it as
//! the harness being inconsistent.
//!
//! Parity: upstream `packages/fs/fs-sandbox`. Upstream fences its two
//! mutations and lets every read through; tetanus fences resolution, which is
//! strictly narrower. `docs/parity-updates/` records that, and
//! [`crate::access`] says why.

use std::path::{Path, PathBuf};

use tetanus_turn::fs::Workspace;

use crate::access::FsMode;
use crate::error::FsError;
use crate::local::{self, LocalFs};
use crate::service::{
    Deleted, DirEntry, EditOutcome, EditRequest, FileSystem, FsInfo, FsTarget, FsVersion,
    WriteIntent, WriteOutcome,
};

/// A filesystem confined to one workspace.
pub struct SandboxedFs {
    fence: Workspace,
    mode: FsMode,
    inner: LocalFs,
    root: PathBuf,
}

impl SandboxedFs {
    /// Fence a backend on `root`, under `mode`.
    ///
    /// `danger-full-access` is refused here rather than silently honoured: a
    /// confining backend asked to confine nothing is a misconfiguration, and
    /// the backend that confines nothing is [`LocalFs`]. [`crate::access::backend`]
    /// is the one place that chooses between them, so a deployment never meets
    /// this error unless it composed the pair by hand.
    pub fn new(root: impl AsRef<Path>, mode: FsMode) -> Result<Self, FsError> {
        let root = root.as_ref();
        if mode == FsMode::DangerFullAccess {
            return Err(FsError::SandboxDenied {
                path: root.display().to_string(),
                operation: "compose a sandboxed filesystem for",
                reason: "\"danger-full-access\" is the absence of a fence, so it selects the \
                         local backend rather than a mode of this one"
                    .into(),
            });
        }
        let fence = Workspace::new(root).map_err(|source| FsError::Io {
            path: root.display().to_string(),
            operation: "resolve of the workspace root",
            message: source.to_string(),
        })?;
        let inner = LocalFs::new(fence.root())?;
        Ok(Self {
            root: fence.root().to_path_buf(),
            fence,
            mode,
            inner,
        })
    }

    /// The canonical workspace root every path is judged against.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Judge one already-resolved target against the mode.
    ///
    /// Taken as one function so no mutation forgets it: `write`, `edit` and
    /// `delete` each call exactly this, and a fourth mutation added later that
    /// does not call it will read wrong beside the three that do.
    fn may_mutate(&self, target: &FsTarget, operation: &'static str) -> Result<(), FsError> {
        if self.mode.may_mutate() {
            return Ok(());
        }
        Err(FsError::SandboxDenied {
            path: target.display().to_string(),
            operation,
            reason: self.mode.refusal(operation),
        })
    }

    /// Resolve a path a caller already holds as a `Path`, answering `None`
    /// when the fence refuses it. The shape [`local::list_dir`] and
    /// [`local::glob_under`] want: they are filtering, not reporting.
    fn contained(&self, path: &Path) -> Option<FsTarget> {
        let resolved = self.fence.resolve(path).ok()?;
        Some(local::target_for(&resolved.path, &self.root))
    }
}

impl FileSystem for SandboxedFs {
    fn backend(&self) -> &'static str {
        "sandboxed"
    }

    fn mode(&self) -> FsMode {
        self.mode
    }

    fn resolve(&self, path: &str) -> Result<FsTarget, FsError> {
        let resolved = self.fence.resolve(path).map_err(|denied| match denied {
            tetanus_turn::fs::FsError::Denied {
                requested,
                resolved,
                root,
            } => FsError::SandboxDenied {
                path: requested,
                operation: "use",
                reason: format!(
                    "it resolves to {resolved}, which is outside the workspace {root}. Work \
                     inside the workspace, using a path relative to it"
                ),
            },
            // The root became unresolvable after the backend was composed -
            // the workspace was deleted or unmounted under the run. It is not
            // a denial and must not read as one: nothing is being refused, the
            // fence has lost its post.
            tetanus_turn::fs::FsError::Root { root, source } => {
                FsError::from_io(Path::new(&root), "resolve of the workspace root", &source)
            }
        })?;
        Ok(local::target_for(&resolved.path, &self.root))
    }

    fn stat(&self, target: &FsTarget) -> Result<Option<FsInfo>, FsError> {
        self.inner.stat(target)
    }

    fn read(&self, target: &FsTarget) -> Result<(String, FsVersion), FsError> {
        self.inner.read(target)
    }

    fn write(
        &self,
        target: &FsTarget,
        content: &str,
        intent: &WriteIntent,
    ) -> Result<WriteOutcome, FsError> {
        self.may_mutate(target, "write")?;
        self.inner.write(target, content, intent)
    }

    fn edit(
        &self,
        target: &FsTarget,
        edit: &EditRequest,
        guard: Option<&FsVersion>,
    ) -> Result<EditOutcome, FsError> {
        self.may_mutate(target, "edit")?;
        self.inner.edit(target, edit, guard)
    }

    fn list(&self, target: &FsTarget) -> Result<Vec<DirEntry>, FsError> {
        local::list_dir(target, &|path| self.contained(path))
    }

    fn glob(&self, base: &FsTarget, pattern: &str) -> Result<Vec<FsTarget>, FsError> {
        local::glob_under(base, pattern, &|path| self.contained(path))
    }

    fn delete(&self, target: &FsTarget, recursive: bool) -> Result<Deleted, FsError> {
        self.may_mutate(target, "delete")?;
        // Deleting the workspace root would leave every later resolution
        // failing on a root that is gone, and no model asking to delete a file
        // meant the root. It is refused as a denial rather than as an I/O
        // error, because it is this build's rule and not the kernel's.
        if target.path() == self.root {
            return Err(FsError::SandboxDenied {
                path: target.display().to_string(),
                operation: "delete",
                reason: "it is the workspace root itself, which this session works inside".into(),
            });
        }
        self.inner.delete(target, recursive)
    }
}
