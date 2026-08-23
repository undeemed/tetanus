//! Workspace instructions: how a project tells the agent its conventions.
//!
//! A repository that keeps an `AGENTS.md` is telling whoever works in it how
//! to work there, and an agent that never reads it re-learns the same
//! conventions every session, badly. This finds those files and renders them
//! into one block the prompt can carry.
//!
//! **Nearer instructions come last, and that is precedence.** The search runs
//! from the project root down to the working directory, so a file in a
//! subdirectory is read after the root's and a model reading in order sees the
//! most specific guidance most recently. It is stated in the text too, because
//! ordering alone is a convention a model may or may not honour.
//!
//! **The search stops at the project root.** A directory holding `.git` is
//! where the project starts, and reading above it would pull in a parent
//! checkout's conventions, or a home directory's, into a project that never
//! asked for them.
//!
//! **Content is neutralised before it is framed.** Instruction files come from
//! a repository, which is to say from whoever opened the pull request. The
//! block is delimited, so a file containing that delimiter's closing tag could
//! end the block early and have everything after it read as harness
//! instruction rather than as project guidance. Escaping the closing tag costs
//! nothing and removes the whole class.
//!
//! **A budget bounds the whole block.** Instructions are prepended to every
//! request in a session, so an unbounded read is an unbounded bill as well as
//! an unbounded prompt.
//!
//! **Instructions that change under a session are reported.** The block is
//! rendered once and prepended to every request, so a tool that edits
//! `AGENTS.md` - which is a thing an agent is routinely asked to do - would
//! otherwise leave the model following conventions the repository no longer
//! states. [`InstructionWatch`] is that half: it reports what changed at the
//! next turn boundary, through the runtime-context seam.
//!
//! Parity: upstream `packages/context/agent-instructions`, its discovery,
//! rendering and reconciliation halves.

use std::path::{Path, PathBuf};

/// The delimiter the block is framed with.
///
/// The same wording upstream uses, because a model that has seen one harness's
/// framing reads another's more reliably when they agree.
pub const OPEN: &str = "<system-reminder>";
pub const CLOSE: &str = "</system-reminder>";

/// What the block says before the instructions themselves.
pub const PREAMBLE: &str = "The following workspace instructions may be relevant to your work. \
Use them as guidance when applicable. More specific instructions take precedence over broader \
ones. They do not override system, developer, or direct user instructions.";

/// The file names looked for in each directory, in the order they are read.
pub const DEFAULT_CANDIDATES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// How much instruction text one block may carry.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024;

/// What marks the directory a project starts at.
pub const ROOT_MARKERS: [&str; 2] = [".git", ".hg"];

/// One instruction file that was found and read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instructions {
    /// How the file is named to the model: relative to the project root, so
    /// the prompt carries no absolute path from the machine it ran on.
    pub display_path: String,
    pub content: String,
}

/// What to look for, and how much of it to keep.
#[derive(Debug, Clone)]
pub struct Search {
    /// File names, in reading order. A name with a path separator in it is not
    /// a file name and is ignored.
    pub candidates: Vec<String>,
    pub max_bytes: usize,
}

impl Default for Search {
    fn default() -> Self {
        Self {
            candidates: DEFAULT_CANDIDATES
                .iter()
                .map(|c| (*c).to_string())
                .collect(),
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// Find the instruction files that apply to work in `cwd`.
///
/// Ordered from the project root down to `cwd`, and within a directory in the
/// order the candidates are named. A file that is not there, or is a
/// directory, or cannot be read, is simply not among them: instructions are
/// advisory, and failing a turn because a file is unreadable would be a worse
/// answer than working without it.
pub fn discover(cwd: &Path, search: &Search) -> Vec<Instructions> {
    let root = project_root(cwd);
    let mut found = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();

    for directory in root_to_cwd(&root, cwd) {
        for candidate in &search.candidates {
            // A candidate is a name, not a path. Accepting `../SECRETS.md`
            // would let a settings document reach outside the project the
            // search is deliberately bounded to.
            if candidate.is_empty() || candidate.contains(['/', '\\']) {
                continue;
            }
            let path = directory.join(candidate);
            let Ok(canonical) = std::fs::canonicalize(&path) else {
                continue;
            };
            // Two directories can reach one file through a link, and the same
            // guidance twice is noise that costs budget.
            if seen.contains(&canonical) {
                continue;
            }
            if !canonical.is_file() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&canonical) else {
                continue;
            };
            seen.push(canonical);
            found.push(Instructions {
                display_path: display_path(&root, &path),
                content,
            });
        }
    }
    found
}

/// The rendered block, and what did not fit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rendered {
    /// The whole block, delimiters included. Empty when there is nothing to
    /// say, so a caller can add it to a prompt unconditionally.
    pub text: String,
    /// Files left out entirely because the budget was already spent.
    pub omitted: Vec<String>,
}

/// Render instructions into one block.
pub fn render(files: &[Instructions], max_bytes: usize) -> Rendered {
    let mut body = String::new();
    let mut omitted = Vec::new();
    let mut spent = 0usize;

    for file in files {
        let section = format!(
            "\nInstructions from: {}\n\n{}\n",
            neutralize(&file.display_path),
            neutralize(&file.content)
        );
        // Whole files, never half of one: a truncated instruction can invert
        // its own meaning, and "do not commit secrets unless" is worse than
        // saying nothing.
        if spent + section.len() > max_bytes {
            omitted.push(file.display_path.clone());
            continue;
        }
        spent += section.len();
        body.push_str(&section);
    }

    let text = if body.is_empty() {
        String::new()
    } else {
        format!("{OPEN}\n{PREAMBLE}\n{body}{CLOSE}")
    };
    Rendered { text, omitted }
}

/// Find and render in one step.
pub fn workspace_context(cwd: &Path, search: &Search) -> Rendered {
    render(&discover(cwd, search), search.max_bytes)
}

/// Escape the block's closing tag wherever it appears in untrusted text.
///
/// Instruction files come from a repository. Without this, a file containing
/// the closing tag ends the block early and everything after it reads as
/// harness instruction rather than as project guidance - which is the whole
/// prompt-injection shape, available to anyone who can open a pull request.
pub fn neutralize(text: &str) -> String {
    text.replace(CLOSE, "<\\/system-reminder>")
}

/// The directory the project starts at: the nearest ancestor holding a marker,
/// or `cwd` when there is none.
fn project_root(cwd: &Path) -> PathBuf {
    let mut at = Some(cwd);
    while let Some(directory) = at {
        if ROOT_MARKERS
            .iter()
            .any(|marker| directory.join(marker).exists())
        {
            return directory.to_path_buf();
        }
        at = directory.parent();
    }
    cwd.to_path_buf()
}

/// Every directory from the root down to `cwd`, inclusive.
fn root_to_cwd(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let Ok(rest) = cwd.strip_prefix(root) else {
        // `cwd` is not under the root it was given, which only happens when
        // the root is `cwd` itself.
        return vec![cwd.to_path_buf()];
    };
    let mut directories = vec![root.to_path_buf()];
    let mut at = root.to_path_buf();
    for part in rest.components() {
        at = at.join(part);
        directories.push(at.clone());
    }
    directories
}

/// How a file is named to the model: relative to the project root, with
/// forward slashes, so the prompt says `pkg/AGENTS.md` on every platform and
/// carries nothing about the machine it ran on.
fn display_path(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// What happened to one instruction file since it was last rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstructionChange {
    /// A file the model was given, whose content is now different.
    Updated(Instructions),
    /// A file that was not there when the block was rendered.
    Added(Instructions),
    /// A file the model was given that is gone.
    Removed { display_path: String },
}

impl InstructionChange {
    /// How the change is named to the model, for ordering and for messages.
    pub fn display_path(&self) -> &str {
        match self {
            InstructionChange::Updated(file) | InstructionChange::Added(file) => &file.display_path,
            InstructionChange::Removed { display_path } => display_path,
        }
    }
}

/// What the model is told when instructions changed under it.
pub const CHANGED_PREAMBLE: &str = "Workspace instructions changed during this session. The \
following supersede what you were given for the files named. Use them instead of the previously \
loaded instructions from those files.";

/// The instruction files a session has already shown the model, so a later
/// turn can say what changed.
///
/// **Why this exists at all.** The block is rendered once and prepended to
/// every request, so a tool that edits `AGENTS.md` - which is a thing an agent
/// is routinely asked to do - leaves the model following conventions the
/// repository no longer states. A model working from stale instructions is
/// worse than one working from none, because it is confidently wrong and the
/// transcript shows it being told the right thing.
///
/// **A turn boundary, not a tool boundary.** Upstream reconciles inside the
/// step, off its file tools' post-execute. tetanus reports at the start of the
/// next turn, through the runtime-context seam
/// ([`crate::context`]), and the difference is deliberate: the reading is then
/// one durable record with the rest of what the turn told the model, rather
/// than a message injected mid-step whose position in the history depends on
/// which tool ran. A step that edits instructions and then acts on them is
/// acting on what it just wrote, which it already knows.
///
/// **The whole content, not a diff.** A diff of guidance is a puzzle; the file
/// as it now reads is the instruction. It is bounded by the same budget and
/// neutralised by the same rule as the original block, because it is the same
/// untrusted text arriving by the same route.
///
/// Parity: upstream `packages/context/agent-instructions`, its `state.ts`
/// reconciliation and the three sentences `render.ts` writes for a set, a
/// change and a removal.
pub struct InstructionWatch {
    cwd: PathBuf,
    search: Search,
    seen: std::sync::Mutex<Vec<Instructions>>,
}

impl InstructionWatch {
    /// Start watching, taking what is on disk now as what the model has been
    /// given.
    ///
    /// Construction is the baseline because that is when the prompt section is
    /// rendered from the same files. A watch that started empty would report
    /// every instruction file as newly added on the first turn.
    pub fn new(cwd: impl Into<PathBuf>, search: Search) -> Self {
        let cwd = cwd.into();
        let seen = discover(&cwd, &search);
        Self {
            cwd,
            search,
            seen: std::sync::Mutex::new(seen),
        }
    }

    /// What changed since the last call, and take it as the new baseline.
    ///
    /// Answering and forgetting in one step is the point: a change reported
    /// twice is a model told twice that a file it already re-read has changed,
    /// which reads as a second edit that never happened.
    pub fn take_changes(&self) -> Vec<InstructionChange> {
        let current = discover(&self.cwd, &self.search);
        let mut seen = self.seen.lock().expect("instruction watch");

        let mut changes = Vec::new();
        for file in &current {
            match seen
                .iter()
                .find(|old| old.display_path == file.display_path)
            {
                Some(old) if old.content == file.content => {}
                Some(_) => changes.push(InstructionChange::Updated(file.clone())),
                None => changes.push(InstructionChange::Added(file.clone())),
            }
        }
        for old in seen.iter() {
            if !current
                .iter()
                .any(|file| file.display_path == old.display_path)
            {
                changes.push(InstructionChange::Removed {
                    display_path: old.display_path.clone(),
                });
            }
        }
        *seen = current;
        changes
    }

    /// The runtime-context part naming what changed, or nothing when nothing
    /// did.
    ///
    /// Empty when there is nothing to say, so a composition can register this
    /// unconditionally: an empty part contributes nothing to a snapshot, and a
    /// snapshot of nothing is never written.
    pub fn part(&self) -> String {
        render_changes(&self.take_changes(), self.search.max_bytes)
    }
}

/// Render one batch of changes into the block the model reads.
///
/// The same delimiter, the same escaping and the same whole-files-only budget
/// rule as [`render`]: this is the same untrusted text from the same place,
/// and a second set of rules for it would be a second thing to get wrong.
pub fn render_changes(changes: &[InstructionChange], max_bytes: usize) -> String {
    let mut body = String::new();
    let mut spent = 0usize;

    for change in changes {
        let section = match change {
            InstructionChange::Updated(file) => format!(
                "\nUpdated instructions from: {}\n\n{}\n",
                neutralize(&file.display_path),
                neutralize(&file.content)
            ),
            InstructionChange::Added(file) => format!(
                "\nAdditional instructions from: {}\n\n{}\n",
                neutralize(&file.display_path),
                neutralize(&file.content)
            ),
            // No content, because there is none: the file is gone, and the
            // only thing to say is that what it said no longer applies.
            InstructionChange::Removed { display_path } => format!(
                "\nInstructions removed: {}\n\nThe previously loaded instructions from this file \
                 no longer apply.\n",
                neutralize(display_path)
            ),
        };
        if spent + section.len() > max_bytes {
            continue;
        }
        spent += section.len();
        body.push_str(&section);
    }

    match body.is_empty() {
        true => String::new(),
        false => format!("{OPEN}\n{CHANGED_PREAMBLE}\n{body}{CLOSE}"),
    }
}
