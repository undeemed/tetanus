//! Shared fixture: a temporary workspace with a backend over it.
//!
//! Every case in this crate works against a real directory on a real disk.
//! A mock filesystem would test the mock: the behaviour under test is
//! canonicalization, atomic replacement and what the operating system reports,
//! and none of those has a faithful double.

#![allow(dead_code)]
// A test binary lints the parts of a shared fixture its own cases do not
// reach, and every suite in this crate reaches a different part of this one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;
use tetanus_fs::service::FileSystem;
use tetanus_fs::{FsMode, LocalFs, SandboxedFs};

/// A workspace on disk, with the files a case needs already in it.
pub struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    /// A workspace and a sibling directory outside it, both inside one
    /// temporary tree so a case that escapes the fence still leaves nothing
    /// behind.
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        // Canonicalized once here so a case comparing paths is not comparing a
        // symlinked temporary root against its resolved form.
        let base = std::fs::canonicalize(dir.path()).expect("canonical root");
        let root = base.join("workspace");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::create_dir_all(&outside).expect("outside");
        Self {
            _dir: dir,
            root,
            outside,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Put a file there, creating whatever directories it needs.
    pub fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(&path, content).expect("seed file");
        path
    }

    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.root.join(relative);
        std::fs::create_dir_all(&path).expect("seed dir");
        path
    }

    pub fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.root.join(relative)).expect("read back")
    }

    pub fn exists(&self, relative: &str) -> bool {
        self.root.join(relative).symlink_metadata().is_ok()
    }

    /// The fenced backend, in the usual mode.
    pub fn sandboxed(&self) -> Arc<dyn FileSystem> {
        Arc::new(SandboxedFs::new(&self.root, FsMode::WorkspaceWrite).expect("sandboxed backend"))
    }

    /// The fenced backend, in a mode the case chooses.
    pub fn in_mode(&self, mode: FsMode) -> Arc<dyn FileSystem> {
        Arc::new(SandboxedFs::new(&self.root, mode).expect("sandboxed backend"))
    }

    /// The unfenced backend, rooted here for relative paths.
    pub fn local(&self) -> Arc<dyn FileSystem> {
        Arc::new(LocalFs::new(&self.root).expect("local backend"))
    }

    /// The sibling directory outside the workspace, for the cases about
    /// escaping it.
    pub fn outside(&self) -> &Path {
        &self.outside
    }
}
