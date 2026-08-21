//! Why a filesystem operation did not happen, as a class rather than as a
//! string.
//!
//! **The class is the point.** A backend that answers `io::Error` makes every
//! caller above it parse a message to find out whether the path was missing,
//! the file was binary, the fence refused, or the disk is full - and a message
//! is not a contract. Upstream reached the same conclusion and gave its
//! filesystem service a closed vocabulary of `FsErrorCode`s
//! (`packages/fs/fs/src/types.ts`); this is that vocabulary, restated as a Rust
//! enum so a caller that forgets a class does not compile.
//!
//! **Every variant carries the two audiences.** [`FsError::code`] is the
//! machine-routable word a permission layer, a retry policy or a wire type
//! branches on; `Display` is the sentence a model reads in a `tool/result` and
//! is written to say what to do next, not merely what went wrong. A model told
//! `FS_NOT_OBSERVED` learns nothing; a model told "read it first, then edit"
//! can act.
//!
//! Parity: upstream `packages/fs/fs/src/types.ts` (`FsError`, `FsErrorCode`).

use std::path::Path;

/// The stable, machine-routable classification of a filesystem failure.
///
/// The words are upstream's, spelled exactly as upstream spells them, so a
/// transcript produced by either harness reads the same way. Two are tetanus's
/// own and are marked as such on their variants: upstream has no delete
/// operation and no glob, so it never had to name their failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsErrorCode {
    NotFound,
    NotDirectory,
    NotText,
    NotRegularFile,
    TooLarge,
    PermissionDenied,
    SandboxDenied,
    IoError,
    StaleVersion,
    NotObserved,
    AmbiguousEdit,
    EditNotFound,
    /// tetanus's own: a delete that would have removed a directory with
    /// children under it, without the caller saying it meant to.
    DirectoryNotEmpty,
    /// tetanus's own: a glob pattern this build cannot read.
    BadPattern,
}

impl FsErrorCode {
    /// The wire spelling. A caller matches on the enum; a journal, a
    /// `tool/result` and an upstream transcript all carry this.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "FS_NOT_FOUND",
            Self::NotDirectory => "FS_NOT_DIRECTORY",
            Self::NotText => "FS_NOT_TEXT",
            Self::NotRegularFile => "FS_NOT_REGULAR_FILE",
            Self::TooLarge => "FS_TOO_LARGE",
            Self::PermissionDenied => "FS_PERMISSION_DENIED",
            Self::SandboxDenied => "FS_SANDBOX_DENIED",
            Self::IoError => "FS_IO_ERROR",
            Self::StaleVersion => "FS_STALE_VERSION",
            Self::NotObserved => "FS_NOT_OBSERVED",
            Self::AmbiguousEdit => "FS_AMBIGUOUS_EDIT",
            Self::EditNotFound => "FS_EDIT_NOT_FOUND",
            Self::DirectoryNotEmpty => "FS_DIRECTORY_NOT_EMPTY",
            Self::BadPattern => "FS_BAD_PATTERN",
        }
    }
}

impl std::fmt::Display for FsErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One filesystem failure, classified.
///
/// The messages are deliberately long. What a model does after a refusal is
/// decided entirely by the sentence it reads, so each one names the path, says
/// which rule refused, and - where there is one - says the move that would
/// work.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("{path}: no such file or directory")]
    NotFound { path: String },

    #[error("{path}: not a directory, so it cannot be listed")]
    NotDirectory { path: String },

    #[error(
        "{path}: not UTF-8 text, so it cannot be read as a string. Read it with a tool that \
         handles bytes, or pick a different file"
    )]
    NotText { path: String },

    #[error("{path}: not a regular file ({kind}), so it cannot be read or written as text")]
    NotRegularFile { path: String, kind: &'static str },

    #[error(
        "{path}: {size} bytes is over the {limit}-byte limit one call may handle. Read a window \
         of it instead of the whole file"
    )]
    TooLarge { path: String, size: u64, limit: u64 },

    #[error("{path}: the operating system refused permission for this {operation}")]
    PermissionDenied {
        path: String,
        operation: &'static str,
    },

    /// The fence refused. Structurally distinct from every other class on
    /// purpose: this build decided, the operating system did not, and a model
    /// should be told which of the two happened rather than left to read it out
    /// of a message.
    #[error("cannot {operation} {path}: {reason}")]
    SandboxDenied {
        path: String,
        operation: &'static str,
        reason: String,
    },

    #[error("{path}: the {operation} failed: {message}")]
    Io {
        path: String,
        operation: &'static str,
        message: String,
    },

    #[error(
        "{path} changed since it was read. Read it again and reapply the change to the current \
         content"
    )]
    StaleVersion { path: String },

    #[error("{path} has not been read in this session. Read it first, then {operation} it")]
    NotObserved {
        path: String,
        operation: &'static str,
    },

    #[error(
        "{path}: the text to replace occurs {count} times. Give more surrounding context so it \
         matches once, or ask for every occurrence to be replaced"
    )]
    AmbiguousEdit { path: String, count: usize },

    #[error("{path}: the text to replace does not occur in the file")]
    EditNotFound { path: String },

    #[error("{path}: the directory is not empty. Say so explicitly to delete it and its contents")]
    DirectoryNotEmpty { path: String },

    #[error("{pattern:?} is not a pattern this build can read: {reason}")]
    BadPattern { pattern: String, reason: String },
}

impl FsError {
    /// The class, for a caller that routes on it.
    pub fn code(&self) -> FsErrorCode {
        match self {
            Self::NotFound { .. } => FsErrorCode::NotFound,
            Self::NotDirectory { .. } => FsErrorCode::NotDirectory,
            Self::NotText { .. } => FsErrorCode::NotText,
            Self::NotRegularFile { .. } => FsErrorCode::NotRegularFile,
            Self::TooLarge { .. } => FsErrorCode::TooLarge,
            Self::PermissionDenied { .. } => FsErrorCode::PermissionDenied,
            Self::SandboxDenied { .. } => FsErrorCode::SandboxDenied,
            Self::Io { .. } => FsErrorCode::IoError,
            Self::StaleVersion { .. } => FsErrorCode::StaleVersion,
            Self::NotObserved { .. } => FsErrorCode::NotObserved,
            Self::AmbiguousEdit { .. } => FsErrorCode::AmbiguousEdit,
            Self::EditNotFound { .. } => FsErrorCode::EditNotFound,
            Self::DirectoryNotEmpty { .. } => FsErrorCode::DirectoryNotEmpty,
            Self::BadPattern { .. } => FsErrorCode::BadPattern,
        }
    }

    /// Classify one `io::Error` met while performing `operation` on `path`.
    ///
    /// The three kinds worth telling apart are told apart here, once, so no
    /// backend has to remember to do it: a missing path, a refusal by the
    /// operating system, and everything else. Everything else keeps the
    /// system's own words, because a caller that has met `ENOSPC` wants to read
    /// "no space left on device" and not "an I/O error occurred".
    pub fn from_io(path: &Path, operation: &'static str, source: &std::io::Error) -> Self {
        let path = path.display().to_string();
        match source.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound { path },
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied { path, operation },
            std::io::ErrorKind::NotADirectory => Self::NotDirectory { path },
            _ => Self::Io {
                path,
                operation,
                message: source.to_string(),
            },
        }
    }
}
