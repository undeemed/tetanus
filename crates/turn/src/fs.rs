//! Path containment: deciding whether a path the model chose is inside the
//! workspace it was given.
//!
//! **This is containment, not a security boundary**, and the distinction is the
//! whole design. The operations a filesystem tool performs are this process's
//! own - open, read, rename - and only the *path* is model-controlled, so
//! canonicalize-then-contain is a complete answer to this surface. Isolating
//! untrusted *code* is a different problem with a different answer (a kernel
//! sandbox), and nothing here should be read as providing it. Upstream draws
//! the same line in `packages/fs/fs-sandbox`, and this is a port of its
//! `containment.ts` plus the path half of its `fs-sandbox.spec.ts`.
//!
//! **Canonicalize first, then compare.** Comparing before resolving is the
//! classic mistake: `workspace/link/secret` is lexically under `workspace` and
//! may be anywhere at all. Every component is resolved as it is walked, so a
//! symlink is followed and then judged, never judged and then followed.
//!
//! **A path that does not exist yet still has to be judged**, because creating
//! a file is the operation most worth fencing. The walk resolves as far as the
//! filesystem goes and keeps the rest as a literal suffix, so a new file under
//! a directory that is a symlink out of the workspace is refused on the
//! strength of the directory, before anything is created.
//!
//! **The residual race is named and accepted.** An ancestor swapped for a
//! symlink between this check and the syscall that follows it is not prevented
//! here; the window is narrowed by resolving immediately before use, and
//! closing it entirely needs the kernel boundary this module is explicitly not.

use std::path::{Component, Path, PathBuf};

/// Why a path was refused.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    /// The path resolved outside the workspace. Structurally distinct from an
    /// I/O failure on purpose: a denial is this build deciding, and a caller -
    /// or a model reading a tool result - should be told which of the two
    /// happened rather than left to read it out of a message.
    #[error("path {requested:?} resolves to {resolved} which is outside the workspace {root}")]
    Denied {
        requested: String,
        resolved: String,
        root: String,
    },
    /// The workspace root itself could not be resolved, so nothing can be
    /// judged against it. Failing here rather than falling back to an
    /// unfenced path is deliberate: a fence that cannot find its post refuses
    /// everything.
    #[error("workspace root {root} could not be resolved: {source}")]
    Root {
        root: String,
        #[source]
        source: std::io::Error,
    },
}

/// A path the model asked for, resolved and judged inside a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The canonical location, symlinks followed as far as the filesystem
    /// goes. This is what an operation should act on - never the requested
    /// spelling, which is what makes the check meaningful.
    pub path: PathBuf,
    /// Whether anything is there right now. `false` authorizes a create and
    /// nothing else; it is not a promise that the path is still absent by the
    /// time a caller acts.
    pub exists: bool,
}

/// A directory the model may name paths inside.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Fix a workspace on a root, resolving it once.
    ///
    /// The root is canonicalized here so every later comparison is between two
    /// resolved paths. A root that does not exist is an error rather than a
    /// workspace that refuses everything, because the two are different
    /// mistakes and only one of them is a deployment's to fix.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, FsError> {
        let root = root.as_ref();
        let canonical = std::fs::canonicalize(root).map_err(|source| FsError::Root {
            root: root.display().to_string(),
            source,
        })?;
        // A regular file canonicalizes perfectly well, so this is a separate
        // question and not one the resolution already answered. A workspace
        // rooted at a file would contain exactly one path and refuse every
        // other, which reads as the fence being broken rather than as the root
        // being wrong - so it is refused where the mistake was made.
        if !canonical.is_dir() {
            return Err(FsError::Root {
                root: root.display().to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotADirectory,
                    "a workspace root must be a directory",
                ),
            });
        }
        Ok(Self { root: canonical })
    }

    /// The canonical root every path is judged against.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve one requested path and judge it.
    ///
    /// A relative path is taken against the root. An absolute one is taken as
    /// it stands and then judged the same way, so naming an absolute path is
    /// not a way around the fence - it is simply a spelling that usually
    /// fails.
    pub fn resolve(&self, requested: impl AsRef<Path>) -> Result<Resolved, FsError> {
        let requested = requested.as_ref();
        let path = self.walk(requested);
        if !self.contains(&path) {
            return Err(FsError::Denied {
                requested: requested.display().to_string(),
                resolved: path.display().to_string(),
                root: self.root.display().to_string(),
            });
        }
        let exists = path.symlink_metadata().is_ok();
        Ok(Resolved { path, exists })
    }

    /// Whether a path this workspace already resolved is the root or under it.
    ///
    /// Two comparisons, in order. The lexical one settles every ordinary
    /// spelling, both sides being canonical by construction. The identity
    /// fallback settles the case where two canonical spellings name one
    /// directory anyway - an alias, a mount, a case-insensitive volume - by
    /// asking the filesystem whether an ancestor *is* the root rather than
    /// whether it is spelled like it. Without it, a workspace reached by one
    /// valid name would refuse paths reached by another.
    fn contains(&self, path: &Path) -> bool {
        if lexically_under(path, &self.root) {
            return true;
        }
        same_file_ancestor(path, &self.root)
    }

    /// Resolve a requested path one component at a time, following symlinks as
    /// they are met and keeping whatever does not exist yet as a literal tail.
    ///
    /// Resolving per component rather than once at the end is what makes the
    /// missing-tail case correct. `root/gone/../link` must not be answered by
    /// normalizing the text to `root/link` and stopping: `link` exists, so it
    /// is resolved the moment the walk reaches it, and a link out of the
    /// workspace is caught. Popping on `..` is sound precisely because what is
    /// being popped is already canonical, so no symlink is being un-followed.
    fn walk(&self, requested: &Path) -> PathBuf {
        let mut current = if requested.is_absolute() {
            PathBuf::from(Component::RootDir.as_os_str())
        } else {
            self.root.clone()
        };

        for component in requested.components() {
            match component {
                Component::Prefix(prefix) => {
                    current = PathBuf::from(prefix.as_os_str());
                }
                Component::RootDir => {
                    // Keep whatever prefix a Windows path established.
                    current.push(Component::RootDir.as_os_str());
                }
                Component::CurDir => {}
                Component::ParentDir => {
                    // At the filesystem root `..` is the root, as every kernel
                    // agrees; `pop` returning false says we are there.
                    current.pop();
                }
                Component::Normal(name) => {
                    current.push(name);
                    // Resolve as soon as there is something to resolve. A
                    // failure means it is not there, and the walk carries the
                    // literal name on so the caller learns where a create
                    // would land.
                    if let Ok(canonical) = std::fs::canonicalize(&current) {
                        current = canonical;
                    }
                }
            }
        }
        current
    }
}

/// Whether `path` is `root` or spelled beneath it.
///
/// The separator on the prefix is what stops `/srv/workspace-old` reading as
/// inside `/srv/workspace`, which a bare `starts_with` on strings would allow.
/// `Path::starts_with` is component-wise and already does this; it is spelled
/// out here because the string form of the same check is a classic escape.
fn lexically_under(path: &Path, root: &Path) -> bool {
    if case_sensitive() {
        return path == root || path.starts_with(root);
    }
    let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
    let (path, root) = (lower(path), lower(root));
    path == root || path.starts_with(&root)
}

/// Whether any existing ancestor of `path` is the same directory as `root`,
/// by the filesystem's own identity rather than by spelling.
#[cfg(unix)]
fn same_file_ancestor(path: &Path, root: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(root_id) = std::fs::metadata(root).map(|m| (m.dev(), m.ino())) else {
        // A root that cannot be read identifies nothing, so nothing is under
        // it. Refusing is the safe direction.
        return false;
    };
    let mut ancestor = Some(path);
    while let Some(current) = ancestor {
        if let Ok(id) = std::fs::metadata(current).map(|m| (m.dev(), m.ino())) {
            if id == root_id {
                return true;
            }
        }
        ancestor = current.parent();
    }
    false
}

/// Without `dev`/`ino` there is no identity to compare, so the lexical answer
/// is the whole answer. That is the conservative direction: a path this build
/// cannot prove is inside is treated as outside.
#[cfg(not(unix))]
fn same_file_ancestor(_path: &Path, _root: &Path) -> bool {
    false
}

/// Whether path comparison here preserves case, following the host convention.
const fn case_sensitive() -> bool {
    !cfg!(windows)
}
