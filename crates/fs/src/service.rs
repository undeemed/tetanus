//! The filesystem seam: the vocabulary every backend speaks, and the trait
//! both of them implement.
//!
//! **A path is resolved once, into an identity.** Every operation takes an
//! [`FsTarget`] rather than a string, because a string is resolved afresh by
//! whoever holds it and two spellings of one file are then two files. The
//! target carries the backend's own key, and two spellings that name one file
//! carry one key - that is what makes a read and the edit that follows it
//! provably about the same thing, which is the whole basis of the observation
//! policy in [`crate::observation`].
//!
//! **The key is opaque.** The local backend happens to use a canonical path;
//! a backend over a remote workspace would use a file id, and a caller that
//! parsed the key would break the day one was mounted. [`FsTarget::display`]
//! is the string to show a model, and [`FsTarget::path`] the one to hand to
//! another OS capability - they are separate for the same reason upstream
//! separates `displayPath` from `processPath`.
//!
//! **A version is a freshness token, not a timestamp.** It answers exactly one
//! question - "is this the file I last saw?" - and it is compared, never
//! interpreted. The local backend derives it from filesystem identity and
//! mtime; nothing above this module may depend on that.
//!
//! **The trait is synchronous, deliberately.** Every operation the two shipped
//! backends perform is a syscall on a local disk, and an `async fn` that never
//! awaits anything is a promise about scheduling that the implementation does
//! not keep. The seam that a remote backend needs is a different one, and it
//! lands with the backend that needs it rather than being guessed at now; the
//! tool bodies in [`crate::tools`] call this from async code the way
//! `crate::instructions` and `tetanus_core::storage` already do.
//!
//! Parity: upstream `packages/fs/fs/src/index.ts` (the `FileSystem` service
//! definition) and `types.ts` (its vocabulary).

use std::path::{Path, PathBuf};

use crate::error::FsError;

/// A path a backend resolved into a stable identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsTarget {
    key: String,
    display: String,
    path: PathBuf,
}

impl FsTarget {
    /// Mint a target. Backends only: a consumer receives targets from
    /// [`FileSystem::resolve`] and never manufactures one, because a
    /// hand-built key would claim an identity nothing checked.
    pub fn new(
        key: impl Into<String>,
        display: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            key: key.into(),
            display: display.into(),
            path: path.into(),
        }
    }

    /// The opaque identity. Compare it; never parse it.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// What to show a model or a user. Workspace-relative where the backend
    /// has a workspace, so a transcript does not leak a home directory.
    pub fn display(&self) -> &str {
        &self.display
    }

    /// The absolute path a subprocess in this backend's world can open.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A freshness token. Equality is the only operation with a meaning.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FsVersion(String);

impl FsVersion {
    /// Backends only, for the reason [`FsTarget::new`] gives.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token as text, for a journal or a wire type that has to carry it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What is at a path, as far as a caller needs to branch on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    /// A symbolic link, reported only by a path-level probe. A resolved target
    /// is never a link: resolution follows it, which is what makes the fence
    /// judge where a link goes rather than where it sits.
    Symlink,
    Other,
}

impl FileKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }
}

/// What [`FileSystem::stat`] answers about a target that exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsInfo {
    pub version: FsVersion,
    pub kind: FileKind,
    /// Byte size of a regular file. Present for directories too when the
    /// platform reports one, and meaningless there.
    pub size: u64,
}

/// One direct child of a listed directory. Metadata only: a listing never
/// reads content, so listing a directory of large files costs one syscall per
/// child rather than the directory's weight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileKind,
    pub size: u64,
    /// The resolved child, for a follow-up operation that does not want to
    /// re-resolve a path it has already been given.
    pub target: FsTarget,
}

/// The guard a write runs under.
///
/// Upstream models the unguarded case by *omitting* the intent; an enum with
/// the third arm named is the same three cases with the third one impossible
/// to reach by accident, which is what a caller wiring a policy wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteIntent {
    /// Create or overwrite, no questions asked. What a deployment that mounts
    /// no observation policy gets.
    Unconditional,
    /// Refuse if anything is already there. The intent a policy derives for a
    /// path the session has never seen: writing it must not silently destroy a
    /// file the model did not know about.
    CreateIfAbsent,
    /// Refuse unless the file is exactly the one that was read.
    ReplaceIfVersion(FsVersion),
}

/// Whether a write put a new file there or replaced one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOperation {
    Create,
    Update,
}

impl WriteOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
        }
    }
}

/// What a write did.
///
/// `before` and `after` are whole contents, never a diff: a renderer computes
/// the diff it wants to show, and an engine that computed one would have
/// decided the presentation question for it. `before` is `None` when the write
/// created the file, and when the prior content was not text this build could
/// carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    pub operation: WriteOperation,
    pub version: FsVersion,
    pub before: Option<String>,
    pub after: String,
}

/// A literal search-and-replace.
///
/// Literal, not a pattern: a model that writes a regular expression by mistake
/// gets a clean "does not occur" rather than an edit somewhere it did not
/// intend. `old` must be non-empty for the same reason - an empty needle
/// matches everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRequest {
    pub old: String,
    pub new: String,
    /// Replace every occurrence. Without it, more than one occurrence is
    /// [`FsError::AmbiguousEdit`] and the file is untouched.
    pub replace_all: bool,
}

/// What an edit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditOutcome {
    pub version: FsVersion,
    pub before: String,
    pub after: String,
    /// How many occurrences were replaced. One unless `replace_all` was set.
    pub replacements: usize,
}

/// What a delete removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deleted {
    pub kind: FileKind,
    /// How many entries went, counting the target itself. One for a file.
    pub entries: usize,
}

/// The largest text one call reads or writes whole.
///
/// A bound has to exist somewhere, and this is the honest place for it: below
/// it, a backend would have to return a truncated string and call it the file.
/// Above it, every tool would need its own limit and they would drift. A model
/// that meets it is told to read a window instead, which is a move it can make.
pub const MAX_TEXT_BYTES: u64 = 8 * 1024 * 1024;

/// The most bytes one [`FileSystem::read_bytes`] window answers with.
///
/// Larger than the text cap on purpose: a picture is bigger than a source file
/// and is still the thing being asked for. It is a bound rather than a
/// promise - a caller that wants more asks twice, which is what a window is
/// for - and it exists so that one call cannot ask a confined worker thread to
/// materialize an arbitrary file in memory.
pub const MAX_WINDOW_BYTES: u64 = 32 * 1024 * 1024;

/// The most entries one glob answers with.
pub const MAX_GLOB_MATCHES: usize = 1000;

/// One execution world's filesystem.
///
/// Implementations own identity, containment and atomicity. Everything above
/// them - read windows, prior-observation guards, tool schemas - is a layer,
/// so a second backend does not have to reimplement a policy to be usable.
pub trait FileSystem: Send + Sync {
    /// The name a diagnostic uses for this backend.
    fn backend(&self) -> &'static str;

    /// What this backend permits a mutation to touch. The tool layer reads it
    /// to say honestly what it can do, rather than finding out by being
    /// refused.
    fn mode(&self) -> crate::access::FsMode;

    /// Resolve a model-supplied path into a target, judging it against
    /// whatever fence this backend has.
    ///
    /// A path that does not exist yet still resolves: creating a file is the
    /// operation most worth judging, and it has to be judged before anything
    /// is created.
    fn resolve(&self, path: &str) -> Result<FsTarget, FsError>;

    /// Metadata, or `None` when nothing is there.
    ///
    /// `None` rather than [`FsError::NotFound`] because absence is an answer to
    /// this question and not a failure of it - a caller deciding whether to
    /// create a file is asking exactly this.
    fn stat(&self, target: &FsTarget) -> Result<Option<FsInfo>, FsError>;

    /// The whole file as text, with its version at the moment it was read.
    ///
    /// The version comes back with the content because that pairing is the
    /// point: a caller that stats and then reads has two moments and a race
    /// between them.
    fn read(&self, target: &FsTarget) -> Result<(String, FsVersion), FsError>;

    /// A window of a file's bytes, with the version it had when it was read.
    ///
    /// Bytes rather than text, and a window rather than the whole file, because
    /// the two limits [`FileSystem::read`] carries are the wrong ones for some
    /// callers: it refuses anything that is not UTF-8, and it refuses anything
    /// past the text cap. A picture is both. So this is the primitive under a
    /// consumer that knows what it is looking at - an image reader, a header
    /// probe, a caller checking a magic number - and `read` stays the one a
    /// model calls, because a model reading raw bytes is a model spending its
    /// context on a hex dump.
    ///
    /// `offset` past the end answers empty rather than failing: asking where a
    /// file ends is a question with an answer, and a caller that windows
    /// through a file meets that boundary on its last read every time.
    fn read_bytes(
        &self,
        target: &FsTarget,
        offset: u64,
        len: u64,
    ) -> Result<(Vec<u8>, FsVersion), FsError>;

    /// Create or replace a file's whole content, atomically.
    fn write(
        &self,
        target: &FsTarget,
        content: &str,
        intent: &WriteIntent,
    ) -> Result<WriteOutcome, FsError>;

    /// Replace literal text inside a file, atomically.
    ///
    /// The guard, the match and the rewrite are one operation on purpose: split
    /// across calls, a caller could check freshness and then write over a
    /// change that landed in between.
    fn edit(
        &self,
        target: &FsTarget,
        edit: &EditRequest,
        guard: Option<&FsVersion>,
    ) -> Result<EditOutcome, FsError>;

    /// Direct children in stable name order.
    fn list(&self, target: &FsTarget) -> Result<Vec<DirEntry>, FsError>;

    /// Every path under `base` matching `pattern`, in stable order.
    fn glob(&self, base: &FsTarget, pattern: &str) -> Result<Vec<FsTarget>, FsError>;

    /// Remove a file, or a directory the caller said it meant to remove.
    fn delete(&self, target: &FsTarget, recursive: bool) -> Result<Deleted, FsError>;
}
