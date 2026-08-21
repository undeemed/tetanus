//! What the harness knows about the project it is working in.
//!
//! A model dropped into a directory knows nothing about it: not where the
//! project starts, not what is in it, not whether the people who work here
//! wrote any of it down. Answering those three costs one tool call and saves
//! the model a dozen `list` calls working it out badly.
//!
//! **The root is found by walking up to a marker, not by trusting the working
//! directory.** A session started three directories deep is still working in
//! one project, and a "root" that is wherever the process happened to start
//! makes every relative path mean something different per session.
//! [`tetanus_turn::instructions::ROOT_MARKERS`] is the same list the
//! instruction search already walks to, so the two cannot disagree about where
//! the project begins.
//!
//! **The layout is a sketch, not a tree.** Top-level entries and the
//! conventional files a project keeps, bounded. A full listing of a large
//! repository is thousands of lines the model pays for and does not read; what
//! it needs is enough to know where to look next, and `glob` is there for the
//! rest.
//!
//! **A path that is not a directory is refused rather than reported empty.**
//! Upstream refuses a nonexistent and a non-directory workspace path for the
//! same reason its skill roots do: absence and refusal are different facts, and
//! a workspace that answers "nothing here" for a path that is a file has told
//! the model something false.
//!
//! Parity: upstream `packages/workspace/workspace`. Most of that package is a
//! *registry* of workspaces for a picker - persisted order, bootstrap from
//! session headers, titles, cwd-drift grouping - which is a surface's state
//! over a store rather than something a turn reads; `docs/parity-updates/`
//! names it. What restates here is what one session knows about the one project
//! it is in.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use tetanus_turn::instructions::{Search, ROOT_MARKERS};
use tetanus_turn::tools::{Tool, ToolError, ToolMode, ToolOutcome, ToolSchema};

/// The most top-level entries a sketch names before it stops.
///
/// A bound, because a repository with four hundred top-level directories is a
/// repository whose listing is not a summary of anything.
pub const MAX_ENTRIES: usize = 60;

/// Why a path cannot be a workspace.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkspaceError {
    #[error("{path}: there is nothing at this path, so it cannot be a workspace")]
    Missing { path: String },
    #[error("{path}: this is a file, not a directory, so it cannot be a workspace")]
    NotADirectory { path: String },
    #[error("{path}: the directory could not be read: {reason}")]
    Unreadable { path: String, reason: String },
}

/// What one session knows about its project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// Where the project starts: the nearest ancestor holding a root marker,
    /// or the working directory when there is none.
    pub root: PathBuf,
    /// Where this session is working, which may be below the root.
    pub cwd: PathBuf,
    /// The marker that decided the root, when one did. `None` means no marker
    /// was found and the working directory is standing in for the root - a
    /// distinction worth carrying, because it is the difference between "this
    /// is a project" and "this is a directory".
    pub marker: Option<String>,
    /// Top-level entries, directories first, then files, each in name order.
    pub entries: Vec<Entry>,
    /// Whether the listing was cut short by [`MAX_ENTRIES`].
    pub truncated: bool,
    /// The instruction files the project keeps, nearest last - the order
    /// `tetanus_turn::instructions` reads them in, so what the model is told
    /// here matches what it was given in the prompt. Named the way the prompt
    /// names them, relative to the root, so neither carries an absolute path
    /// off the machine it ran on.
    pub instructions: Vec<String>,
}

/// One top-level entry of the sketch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub directory: bool,
}

/// Find the project root above `cwd`, and the marker that decided it.
///
/// The walk stops at the first marker, so a checkout inside a checkout resolves
/// to the inner one - which is the project the person is working in.
pub fn find_root(cwd: &Path) -> (PathBuf, Option<String>) {
    for ancestor in cwd.ancestors() {
        for marker in ROOT_MARKERS {
            if ancestor.join(marker).exists() {
                return (ancestor.to_path_buf(), Some(marker.to_string()));
            }
        }
    }
    (cwd.to_path_buf(), None)
}

/// Read what is knowable about the project `cwd` sits in.
pub fn describe(cwd: &Path) -> Result<Workspace, WorkspaceError> {
    let named = || cwd.display().to_string();
    let meta = std::fs::metadata(cwd).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => WorkspaceError::Missing { path: named() },
        _ => WorkspaceError::Unreadable {
            path: named(),
            reason: error.to_string(),
        },
    })?;
    if !meta.is_dir() {
        return Err(WorkspaceError::NotADirectory { path: named() });
    }
    // Canonicalized so two spellings of one workspace - a symlinked checkout,
    // a relative path - are one workspace. Upstream dedupes its registry by
    // canonical path for the same reason.
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let (root, marker) = find_root(&cwd);

    let reader = std::fs::read_dir(&root).map_err(|error| WorkspaceError::Unreadable {
        path: root.display().to_string(),
        reason: error.to_string(),
    })?;
    let mut directories = Vec::new();
    let mut files = Vec::new();
    for child in reader.flatten() {
        let name = child.file_name().to_string_lossy().to_string();
        // A dot entry is machinery - `.git`, `.cache` - and naming forty of
        // them crowds out the twelve entries that say what the project is.
        if name.starts_with('.') {
            continue;
        }
        let directory = child.path().is_dir();
        if directory {
            directories.push(Entry { name, directory });
        } else {
            files.push(Entry { name, directory });
        }
    }
    directories.sort_by(|a, b| a.name.cmp(&b.name));
    files.sort_by(|a, b| a.name.cmp(&b.name));

    let total = directories.len() + files.len();
    let mut entries: Vec<Entry> = directories.into_iter().chain(files).collect();
    entries.truncate(MAX_ENTRIES);

    Ok(Workspace {
        instructions: tetanus_turn::instructions::discover(&cwd, &Search::default())
            .into_iter()
            .map(|found| found.display_path)
            .collect(),
        root,
        cwd,
        marker,
        entries,
        truncated: total > MAX_ENTRIES,
    })
}

impl Workspace {
    /// The sketch as the model reads it.
    ///
    /// Prose rather than JSON, because every line of it is something the model
    /// acts on directly and none of it is a value it needs to parse.
    pub fn render(&self) -> String {
        let mut out = format!("Project root: {}\n", self.root.display());
        if self.cwd != self.root {
            out.push_str(&format!("Working directory: {}\n", self.cwd.display()));
        }
        match &self.marker {
            Some(marker) => out.push_str(&format!("Identified by: {marker}\n")),
            None => out.push_str(
                "No repository marker was found above this directory, so the working directory \
                 is being treated as the project root.\n",
            ),
        }

        if self.instructions.is_empty() {
            out.push_str("Instruction files: none\n");
        } else {
            out.push_str("Instruction files (nearest last, already in your prompt):\n");
            for path in &self.instructions {
                out.push_str(&format!("  {path}\n"));
            }
        }

        out.push_str("Top level:\n");
        for entry in &self.entries {
            match entry.directory {
                true => out.push_str(&format!("  {}/\n", entry.name)),
                false => out.push_str(&format!("  {}\n", entry.name)),
            }
        }
        if self.truncated {
            out.push_str(&format!(
                "  ... more than {MAX_ENTRIES} entries; use glob to look further\n"
            ));
        }
        out
    }
}

/// The tool that answers where the model is.
pub struct WorkspaceInfoTool {
    cwd: PathBuf,
}

impl WorkspaceInfoTool {
    pub const NAME: &'static str = "workspace_info";

    pub fn new(cwd: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self { cwd: cwd.into() })
    }
}

#[async_trait::async_trait]
impl Tool for WorkspaceInfoTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Say where this session is working: the project root, how it was \
                          identified, the instruction files the project keeps, and what is at the \
                          top level."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, _arguments: &Value) -> Result<ToolOutcome, ToolError> {
        // A workspace that cannot be described is told to the model as a
        // result rather than raised: the working directory being gone is
        // something it can work around by naming absolute paths, and failing
        // the step would only end the turn.
        Ok(match describe(&self.cwd) {
            Ok(workspace) => ToolOutcome::ok(workspace.render()),
            Err(refused) => ToolOutcome::failed(refused.to_string()),
        })
    }
}
