//! Picking a workspace directory, for a client that cannot reach a chooser.
//!
//! Upstream makes this a capability seam: `capability()` answers a union, and
//! a consumer switches on its kind. `native` opens an OS dialog on the host's
//! display; `browse` answers listings and creations so the client can draw its
//! own chooser. A remote browser has no reach into the host's display, so a
//! host serving one implements `browse` - which is what this is.
//!
//! The seam is worth keeping even with one backend behind it, and upstream
//! says why: "for an unknown kind, consumers hide directory picking rather
//! than fail". A surface that asks what it may do can be older or newer than
//! the host it is talking to and still work.
//!
//! # What a listing is, and what it is not
//!
//! Directories only, name-sorted, one level. Not a tree, not a search, and
//! never a file: this answers "where would you like to work", and every file
//! in the answer is a row the reader has to skip past to find out.
//!
//! A symlink to a directory is followed, and one that goes nowhere is left
//! out, because the probe failing is exactly what "not enterable" means.
//! Hidden is reported and not applied - the dot convention is the host's fact
//! and whether to show it is the reader's choice.

use std::path::{Path, PathBuf};

/// What a host can do about choosing a directory.
///
/// One variant today. It is an enum rather than a bare struct so that a
/// consumer written against it keeps compiling - and keeps hiding the feature
/// rather than failing - when a host answers with a kind it has never heard
/// of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Listing and creation, for a chooser the client draws itself.
    Browse,
}

/// One row of a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    /// The POSIX dot convention, reported rather than acted on: display
    /// policy is the client's.
    pub hidden: bool,
}

/// One level, and how to get back up it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    /// The ancestor chain from the filesystem root, every one a jump target.
    pub crumbs: Vec<Entry>,
    /// Whether the level had more rows than the bound allows.
    pub truncated: bool,
}

/// What can go wrong, in the three shapes upstream's wire codes name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerError {
    /// The path could not be read, or was not a fully qualified one.
    Unreadable(PathBuf),
    /// A directory of that name is already there.
    Exists(PathBuf),
    /// It could not be made: a missing parent, a refused permission.
    CreateFailed(PathBuf),
}

impl std::fmt::Display for PickerError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PickerError::Unreadable(path) => write!(out, "{} cannot be read", path.display()),
            PickerError::Exists(path) => write!(out, "{} is already there", path.display()),
            PickerError::CreateFailed(path) => write!(out, "{} could not be made", path.display()),
        }
    }
}

impl std::error::Error for PickerError {}

/// The most rows one level answers with.
///
/// Upstream's default, and its reasoning: it is the bound GitHub's web UI puts
/// on a directory listing, which is enough for every real tree and small
/// enough that a directory with a million children cannot be used to make a
/// host allocate a million rows.
pub const MAX_ENTRIES: usize = 1000;

/// The in-app browsing backend.
#[derive(Debug, Clone, Copy, Default)]
pub struct Browse {
    /// The bound on one level, so a caller with a smaller screen can ask for
    /// less than [`MAX_ENTRIES`].
    pub max_entries: Option<usize>,
}

impl Browse {
    /// What this host can do, which is the question a consumer asks first.
    pub fn capability(&self) -> Capability {
        Capability::Browse
    }

    /// One level, name-sorted, directories only.
    ///
    /// No path means the account's home directory, because a chooser that
    /// opens at the filesystem root makes every reader walk down to where they
    /// live.
    pub fn list(&self, path: Option<&Path>) -> Result<Listing, PickerError> {
        let path = match path {
            Some(path) => qualified(path)?,
            None => home(),
        };
        let read = std::fs::read_dir(&path).map_err(|_| PickerError::Unreadable(path.clone()))?;
        let bound = self.max_entries.unwrap_or(MAX_ENTRIES);

        let mut named: Vec<(String, PathBuf)> = Vec::new();
        let mut over = false;
        for row in read.flatten() {
            let name = row.file_name().to_string_lossy().to_string();
            named.push((name, row.path()));
            // Sorted and cut as it goes, so a directory with a million
            // children costs the bound and not the directory.
            if named.len() > bound * 2 {
                named.sort_by(|left, right| left.0.cmp(&right.0));
                named.truncate(bound);
                over = true;
            }
        }
        named.sort_by(|left, right| left.0.cmp(&right.0));
        let truncated = over || named.len() > bound;
        named.truncate(bound);

        // The probe is last and only on what survived the cut: `stat` on every
        // child of a huge directory is the cost this bound exists to avoid.
        // A link that goes nowhere fails it, which is what "not enterable"
        // means, and it is left out rather than shown as a dead end.
        let entries = named
            .into_iter()
            .filter(|(_, path)| path.is_dir())
            .map(|(name, path)| Entry {
                hidden: name.starts_with('.'),
                name,
                path,
            })
            .collect();

        Ok(Listing {
            crumbs: crumbs(&path),
            path,
            entries,
            truncated,
        })
    }

    /// Make one child directory.
    ///
    /// Not recursive: a missing parent is a real failure and not a level to
    /// invent, because a reader who mistyped one segment of a path should be
    /// told, not given a tree they did not ask for.
    pub fn create(&self, path: &Path, name: &str) -> Result<Entry, PickerError> {
        let parent = qualified(path)?;
        // One non-blank segment, checked here as well as at the wire, because
        // a backend called directly has the same rule as one called across a
        // socket.
        let bad = name.trim().is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name == "."
            || name == "..";
        if bad {
            return Err(PickerError::CreateFailed(parent.join(name)));
        }
        let made = parent.join(name);
        if made.exists() {
            return Err(PickerError::Exists(made));
        }
        match std::fs::create_dir(&made) {
            Ok(()) => Ok(Entry {
                hidden: name.starts_with('.'),
                name: name.to_string(),
                path: made,
            }),
            Err(_) => Err(PickerError::CreateFailed(made)),
        }
    }
}

/// A path this backend will act on, or a refusal.
///
/// Fully qualified only. A relative path would be rebased under whatever
/// directory the host process happens to be in, which is a different place
/// from the one the reader named and is nobody's intent.
fn qualified(path: &Path) -> Result<PathBuf, PickerError> {
    match path.is_absolute() {
        true => Ok(path.to_path_buf()),
        false => Err(PickerError::Unreadable(path.to_path_buf())),
    }
}

/// The account's home, or the root if there is not one.
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The ancestor chain, root first, every one a place to jump to.
///
/// The root crumb is labelled by its full path rather than by an empty name,
/// because a crumb with no text is a target nobody can click.
fn crumbs(path: &Path) -> Vec<Entry> {
    let mut chain: Vec<Entry> = Vec::new();
    let mut walked = PathBuf::new();
    for part in path.components() {
        walked.push(part);
        let name = match part {
            std::path::Component::RootDir => walked.display().to_string(),
            part => part.as_os_str().to_string_lossy().to_string(),
        };
        chain.push(Entry {
            hidden: name.starts_with('.'),
            name,
            path: walked.clone(),
        });
    }
    chain
}
