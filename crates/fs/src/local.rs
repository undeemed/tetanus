//! The local backend: this process's own syscalls, with no fence.
//!
//! It is the `danger-full-access` backend of [`crate::access`], and it is also
//! the body of the fenced one - [`crate::SandboxedFs`] judges a path and then
//! hands the work here, so there is one implementation of "read a file" and
//! not two that drift.
//!
//! **Resolution is the containment walk, rooted at the filesystem root.**
//! Rather than write a second path walker, this reuses
//! [`tetanus_turn::fs::Workspace`] with `/` as its root: a fence that contains
//! everything is not a fence, and the walk - canonicalize per component,
//! follow a symlink before judging it, keep the part that does not exist yet as
//! a literal tail - is exactly the behaviour a local backend needs anyway. The
//! two backends therefore share one tested walk, and the difference between
//! them is only which root it is judged against.
//!
//! **Mutations are atomic.** A write lands in a temporary file beside its
//! destination, is flushed to the disk, and is then renamed over it - so a
//! reader sees the old content or the new one and never a half-written file,
//! and a crash mid-write loses the write rather than the file.
//!
//! Parity: upstream `packages/fs/fs-local`, pinned by its `filesystem.spec.ts`
//! and `fsio.spec.ts`.

use std::io::Write;
use std::path::{Path, PathBuf};

use tetanus_turn::fs::Workspace;

use crate::access::FsMode;
use crate::error::FsError;
use crate::glob::Pattern;
use crate::service::{
    Deleted, DirEntry, EditOutcome, EditRequest, FileKind, FileSystem, FsInfo, FsTarget, FsVersion,
    WriteIntent, WriteOperation, WriteOutcome, MAX_GLOB_MATCHES, MAX_TEXT_BYTES,
};

/// The most directory entries one glob visits before it stops walking.
///
/// A bound is needed because a pattern is model-supplied and a home directory
/// is large. Reaching it answers with what was found rather than failing: a
/// partial answer to "where is the config file" is useful, and a refusal is
/// not.
const MAX_GLOB_VISITS: usize = 50_000;

/// The unfenced filesystem. Relative paths are taken against `root`.
pub struct LocalFs {
    /// Where a relative path starts. Not a fence: it is the working directory,
    /// and an absolute path ignores it.
    root: PathBuf,
    /// The whole-filesystem walk. Held rather than rebuilt per call because
    /// constructing it canonicalizes, which is a syscall.
    walk: Workspace,
}

impl LocalFs {
    /// Root a local backend at a working directory.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, FsError> {
        let root = root.as_ref();
        let canonical = std::fs::canonicalize(root).map_err(|source| {
            FsError::from_io(root, "resolve of the working directory", &source)
        })?;
        if !canonical.is_dir() {
            return Err(FsError::NotDirectory {
                path: canonical.display().to_string(),
            });
        }
        let walk = Workspace::new(std::path::Component::RootDir.as_os_str()).map_err(|source| {
            FsError::Io {
                path: "/".into(),
                operation: "resolve of the filesystem root",
                message: source.to_string(),
            }
        })?;
        Ok(Self {
            root: canonical,
            walk,
        })
    }

    /// The working directory relative paths are taken against.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve one path with no fence, absolutizing a relative one against the
    /// working directory first.
    pub(crate) fn resolve_unfenced(&self, path: &str) -> Result<FsTarget, FsError> {
        let requested = Path::new(path);
        let absolute = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.root.join(requested)
        };
        // The walk is rooted at `/`, so it resolves and never denies. The
        // `Denied` arm is therefore unreachable rather than ignored, and it is
        // reported as what it would be - this build's own mistake - instead of
        // being mapped onto a filesystem error the caller would misread.
        let resolved = self.walk.resolve(&absolute).map_err(|source| FsError::Io {
            path: absolute.display().to_string(),
            operation: "resolve",
            message: source.to_string(),
        })?;
        Ok(target_for(&resolved.path, &resolved.path))
    }
}

/// Build a target, showing it relative to `base` where it is under it.
///
/// A model reading `src/main.rs` learns more than one reading
/// `/home/someone/checkout/src/main.rs`, and a transcript that carries the
/// second has leaked a home directory into a log somebody will paste
/// somewhere.
pub(crate) fn target_for(path: &Path, base: &Path) -> FsTarget {
    let display = match path.strip_prefix(base) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_string(),
        Ok(relative) => relative.display().to_string(),
        Err(_) => path.display().to_string(),
    };
    // The key is the canonical path, which is what makes two spellings of one
    // file one identity. It is opaque by contract: nothing above the backend
    // may read it as a path.
    FsTarget::new(path.display().to_string(), display, path)
}

/// The freshness token of one metadata reading.
///
/// Identity plus mtime plus size. Identity alone would call a file that was
/// replaced in place unchanged; mtime alone would call a file restored from a
/// backup with its timestamp unchanged; together they answer the only question
/// a version is asked - "is this the file I last saw?" - for every change a
/// coding agent makes.
pub(crate) fn version_of(meta: &std::fs::Metadata) -> FsVersion {
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()))
        .unwrap_or_else(|| "0".to_string());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FsVersion::new(format!(
            "{}:{}:{stamp}:{}",
            meta.dev(),
            meta.ino(),
            meta.len()
        ))
    }
    #[cfg(not(unix))]
    {
        FsVersion::new(format!("{stamp}:{}", meta.len()))
    }
}

pub(crate) fn kind_of(meta: &std::fs::Metadata) -> FileKind {
    if meta.is_file() {
        FileKind::File
    } else if meta.is_dir() {
        FileKind::Directory
    } else if meta.is_symlink() {
        FileKind::Symlink
    } else {
        FileKind::Other
    }
}

/// Metadata, or `None` when nothing is there.
pub(crate) fn stat_path(path: &Path) -> Result<Option<FsInfo>, FsError> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(Some(FsInfo {
            version: version_of(&meta),
            kind: kind_of(&meta),
            size: meta.len(),
        })),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(FsError::from_io(path, "stat", &source)),
    }
}

/// Read a regular text file whole, with the version it had when it was read.
pub(crate) fn read_text(target: &FsTarget) -> Result<(String, FsVersion), FsError> {
    let path = target.path();
    let meta = std::fs::metadata(path).map_err(|source| FsError::from_io(path, "read", &source))?;
    let kind = kind_of(&meta);
    if kind != FileKind::File {
        return Err(match kind {
            FileKind::Directory => FsError::NotRegularFile {
                path: target.display().to_string(),
                kind: "directory",
            },
            _ => FsError::NotRegularFile {
                path: target.display().to_string(),
                kind: "special file",
            },
        });
    }
    if meta.len() > MAX_TEXT_BYTES {
        return Err(FsError::TooLarge {
            path: target.display().to_string(),
            size: meta.len(),
            limit: MAX_TEXT_BYTES,
        });
    }
    let bytes = std::fs::read(path).map_err(|source| FsError::from_io(path, "read", &source))?;
    let text = String::from_utf8(bytes).map_err(|_| FsError::NotText {
        path: target.display().to_string(),
    })?;
    Ok((text, version_of(&meta)))
}

/// The content a write reports as `before`, or `None` when there is none this
/// build can carry.
///
/// A prior file that is binary, or too large, is not an error here: the write
/// is still allowed to proceed, and what is lost is the diff basis a renderer
/// would have used. Refusing the write over it would make a tool unable to
/// overwrite a file it can legitimately replace.
fn prior_text(target: &FsTarget) -> Option<String> {
    read_text(target).ok().map(|(text, _)| text)
}

/// Create or replace a file's whole content atomically, under a guard.
pub(crate) fn write_text(
    target: &FsTarget,
    content: &str,
    intent: &WriteIntent,
) -> Result<WriteOutcome, FsError> {
    let path = target.path();
    let existing = stat_path(path)?;
    if let Some(info) = &existing {
        if info.kind != FileKind::File {
            return Err(FsError::NotRegularFile {
                path: target.display().to_string(),
                kind: match info.kind {
                    FileKind::Directory => "directory",
                    _ => "special file",
                },
            });
        }
    }

    match intent {
        WriteIntent::Unconditional => {}
        WriteIntent::CreateIfAbsent => {
            if existing.is_some() {
                // Upstream answers this with `FS_NOT_OBSERVED`, and the code is
                // right: the intent was derived because the session had never
                // read the file, so what is wrong is not the write but that it
                // is blind.
                return Err(FsError::NotObserved {
                    path: target.display().to_string(),
                    operation: "overwrite",
                });
            }
        }
        WriteIntent::ReplaceIfVersion(expected) => match &existing {
            Some(info) if &info.version == expected => {}
            _ => {
                return Err(FsError::StaleVersion {
                    path: target.display().to_string(),
                })
            }
        },
    }

    let before = existing.as_ref().and_then(|_| prior_text(target));
    publish(path, content)?;
    let after = stat_path(path)?.ok_or_else(|| FsError::Io {
        path: target.display().to_string(),
        operation: "write",
        message: "the file was gone immediately after it was written".into(),
    })?;
    Ok(WriteOutcome {
        operation: match existing {
            Some(_) => WriteOperation::Update,
            None => WriteOperation::Create,
        },
        version: after.version,
        before,
        after: content.to_string(),
    })
}

/// Replace literal text inside a file, atomically, under a freshness guard.
pub(crate) fn edit_text(
    target: &FsTarget,
    edit: &EditRequest,
    guard: Option<&FsVersion>,
) -> Result<EditOutcome, FsError> {
    if edit.old.is_empty() {
        return Err(FsError::BadPattern {
            pattern: String::new(),
            reason: "the text to replace must not be empty, because an empty string occurs \
                     between every pair of characters"
                .into(),
        });
    }
    let (before, version) = read_text(target)?;
    // The guard is checked against the content this call just read, so the
    // window between checking and rewriting is this function and nothing
    // wider.
    if let Some(expected) = guard {
        if &version != expected {
            return Err(FsError::StaleVersion {
                path: target.display().to_string(),
            });
        }
    }

    let count = before.matches(&edit.old).count();
    if count == 0 {
        return Err(FsError::EditNotFound {
            path: target.display().to_string(),
        });
    }
    if count > 1 && !edit.replace_all {
        return Err(FsError::AmbiguousEdit {
            path: target.display().to_string(),
            count,
        });
    }
    let (after, replacements) = if edit.replace_all {
        (before.replace(&edit.old, &edit.new), count)
    } else {
        (before.replacen(&edit.old, &edit.new, 1), 1)
    };

    publish(target.path(), &after)?;
    let stamped = stat_path(target.path())?.ok_or_else(|| FsError::Io {
        path: target.display().to_string(),
        operation: "edit",
        message: "the file was gone immediately after it was edited".into(),
    })?;
    Ok(EditOutcome {
        version: stamped.version,
        before,
        after,
        replacements,
    })
}

/// Write `content` where `path` is, without a reader ever seeing a partial
/// file.
///
/// Temporary beside the destination, not in a temp directory: a rename is
/// atomic only within one filesystem, and a `/tmp` on another mount would turn
/// the publish into a copy that can be interrupted. The temporary is removed on
/// every failure path, so a refused write leaves nothing behind.
///
/// The file's own bytes are flushed before the rename. The directory entry is
/// not: a crash in that window loses the write, which is the same outcome as a
/// crash one instant earlier, and paying a directory fsync per tool call to
/// narrow it is not a trade this surface needs.
fn publish(path: &Path, content: &str) -> Result<(), FsError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let temp = parent.join(format!(".{name}.tetanus-{}.tmp", stamp()));

    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()
    })();
    if let Err(source) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(FsError::from_io(path, "write", &source));
    }
    if let Err(source) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(FsError::from_io(path, "write", &source));
    }
    Ok(())
}

/// Enough to keep two writes in the same directory from colliding on a
/// temporary name: the clock, at nanosecond resolution, plus the thread.
fn stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{now:x}-{:?}", std::thread::current().id())
        .replace(['(', ')', ' '], "")
        .replace("ThreadId", "t")
}

/// Direct children in name order, each resolved by `resolve`.
///
/// The resolver is passed in rather than assumed, because a fenced backend has
/// to judge each child: a listing must never name a path the fence would refuse
/// to open, or the model is invited to try something that cannot work.
pub(crate) fn list_dir(
    target: &FsTarget,
    resolve: &dyn Fn(&Path) -> Option<FsTarget>,
) -> Result<Vec<DirEntry>, FsError> {
    let path = target.path();
    let meta = std::fs::metadata(path).map_err(|source| FsError::from_io(path, "list", &source))?;
    if !meta.is_dir() {
        return Err(FsError::NotDirectory {
            path: target.display().to_string(),
        });
    }
    let reader =
        std::fs::read_dir(path).map_err(|source| FsError::from_io(path, "list", &source))?;

    let mut entries = Vec::new();
    for child in reader {
        let child = child.map_err(|source| FsError::from_io(path, "list", &source))?;
        let name = child.file_name().to_string_lossy().to_string();
        // `symlink_metadata`, so a link is reported as a link rather than as
        // whatever it points at. A caller that wants the destination resolves
        // the child target, which follows it.
        let meta = match child.path().symlink_metadata() {
            Ok(meta) => meta,
            // A child that vanished between the read and the stat is not an
            // error: a directory listing is a snapshot of a moving thing, and
            // failing the whole call over one entry would make listing a busy
            // directory unreliable for no gain.
            Err(_) => continue,
        };
        let Some(resolved) = resolve(&child.path()) else {
            continue;
        };
        entries.push(DirEntry {
            name,
            kind: kind_of(&meta),
            size: meta.len(),
            target: resolved,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// Every path under `base` matching `pattern`, in stable order.
pub(crate) fn glob_under(
    base: &FsTarget,
    pattern: &str,
    resolve: &dyn Fn(&Path) -> Option<FsTarget>,
) -> Result<Vec<FsTarget>, FsError> {
    let pattern = Pattern::parse(pattern)?;
    let root = base.path().to_path_buf();
    let mut found = Vec::new();
    let mut visits = 0usize;
    let mut stack = vec![(root.clone(), Vec::<String>::new())];

    while let Some((dir, prefix)) = stack.pop() {
        let Ok(reader) = std::fs::read_dir(&dir) else {
            // A directory this process cannot read contributes nothing. The
            // walk is a search, and a search that fails because one subtree is
            // unreadable answers nothing useful.
            continue;
        };
        for child in reader.flatten() {
            if visits >= MAX_GLOB_VISITS || found.len() >= MAX_GLOB_MATCHES {
                break;
            }
            visits += 1;
            let name = child.file_name().to_string_lossy().to_string();
            if name.starts_with('.') && !pattern.wants_hidden() {
                continue;
            }
            let mut parts = prefix.clone();
            parts.push(name);
            let Ok(meta) = child.path().symlink_metadata() else {
                continue;
            };
            if pattern.matches(&parts) {
                if let Some(target) = resolve(&child.path()) {
                    found.push(target);
                }
            }
            // A directory symlink is not followed. Following one turns a walk
            // into a graph traversal with cycles in it, and the loop it finds
            // would be a hang rather than an error.
            if meta.is_dir() && !meta.is_symlink() {
                stack.push((child.path(), parts));
            }
        }
    }

    found.sort_by(|a, b| a.path().cmp(b.path()));
    found.dedup_by(|a, b| a.path() == b.path());
    Ok(found)
}

/// Remove a file, or a directory the caller said it meant to remove.
pub(crate) fn delete_path(target: &FsTarget, recursive: bool) -> Result<Deleted, FsError> {
    let path = target.path();
    let meta = std::fs::symlink_metadata(path)
        .map_err(|source| FsError::from_io(path, "delete", &source))?;
    let kind = kind_of(&meta);
    if kind != FileKind::Directory {
        std::fs::remove_file(path).map_err(|source| FsError::from_io(path, "delete", &source))?;
        return Ok(Deleted { kind, entries: 1 });
    }

    let children = count_entries(path)?;
    if children > 1 && !recursive {
        return Err(FsError::DirectoryNotEmpty {
            path: target.display().to_string(),
        });
    }
    if recursive {
        std::fs::remove_dir_all(path)
            .map_err(|source| FsError::from_io(path, "delete", &source))?;
    } else {
        std::fs::remove_dir(path).map_err(|source| FsError::from_io(path, "delete", &source))?;
    }
    Ok(Deleted {
        kind,
        entries: children,
    })
}

/// How many entries a delete would remove, counting the directory itself.
fn count_entries(path: &Path) -> Result<usize, FsError> {
    let mut total = 1;
    let reader =
        std::fs::read_dir(path).map_err(|source| FsError::from_io(path, "delete", &source))?;
    for child in reader.flatten() {
        let Ok(meta) = child.path().symlink_metadata() else {
            continue;
        };
        if meta.is_dir() && !meta.is_symlink() {
            total += count_entries(&child.path())?;
        } else {
            total += 1;
        }
    }
    Ok(total)
}

impl FileSystem for LocalFs {
    fn backend(&self) -> &'static str {
        "local"
    }

    fn mode(&self) -> FsMode {
        FsMode::DangerFullAccess
    }

    fn resolve(&self, path: &str) -> Result<FsTarget, FsError> {
        self.resolve_unfenced(path)
    }

    fn stat(&self, target: &FsTarget) -> Result<Option<FsInfo>, FsError> {
        stat_path(target.path())
    }

    fn read(&self, target: &FsTarget) -> Result<(String, FsVersion), FsError> {
        read_text(target)
    }

    fn write(
        &self,
        target: &FsTarget,
        content: &str,
        intent: &WriteIntent,
    ) -> Result<WriteOutcome, FsError> {
        write_text(target, content, intent)
    }

    fn edit(
        &self,
        target: &FsTarget,
        edit: &EditRequest,
        guard: Option<&FsVersion>,
    ) -> Result<EditOutcome, FsError> {
        edit_text(target, edit, guard)
    }

    fn list(&self, target: &FsTarget) -> Result<Vec<DirEntry>, FsError> {
        list_dir(target, &|path| Some(target_for(path, &self.root)))
    }

    fn glob(&self, base: &FsTarget, pattern: &str) -> Result<Vec<FsTarget>, FsError> {
        glob_under(base, pattern, &|path| Some(target_for(path, &self.root)))
    }

    fn delete(&self, target: &FsTarget, recursive: bool) -> Result<Deleted, FsError> {
        delete_path(target, recursive)
    }
}
