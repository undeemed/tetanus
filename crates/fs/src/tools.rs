//! The model-facing filesystem tools.
//!
//! Seven tools over one [`FileSystem`], registered into the ordinary
//! [`ToolRegistry`] and dispatched by the ordinary tool pipeline. They own
//! schemas, argument checking, read windows, the wording of a result, and the
//! observation events - never a backend, which is why swapping the fenced
//! backend for the unfenced one changes nothing here.
//!
//! **A refusal is a result, not a failure.** Every [`FsError`] comes back as a
//! `tool/result` with `ok: false` whose content is the error's class followed
//! by its sentence. The class is there for a surface that routes on it; the
//! sentence is there because the model's next move is decided entirely by what
//! it reads. A tool that failed the *turn* on a denied path would leave the
//! model unable to try something else, which is the opposite of what a fence is
//! for.
//!
//! **What a tool sees is what it observed.** Read, stat and write record an
//! observation on the shared [`ObservedState`], and write and edit derive their
//! guards from it. A deployment that wants the bare provider behaviour
//! composes [`FsTools::unobserved`] and gets unconditional mutation, which is
//! upstream's "without this plugin" case.
//!
//! Parity: upstream `packages/fs/tool-fs` (read, write, edit) and
//! `packages/fs/tool-fs-search` (glob), restated against
//! [`tetanus_turn::tools`].

use std::sync::Arc;

use serde_json::{json, Value};
use tetanus_turn::tools::{
    Permission, Tool, ToolError, ToolMode, ToolOutcome, ToolRegistry, ToolSchema,
};

use crate::error::FsError;
use crate::observation::{Observation, ObservedState};
use crate::service::{
    DirEntry, EditRequest, FileKind, FileSystem, FsTarget, WriteIntent, MAX_GLOB_MATCHES,
};

/// The default and maximum number of lines one `read` answers with.
pub const READ_LIMIT: usize = 2000;
/// The most characters of one line a read renders before it truncates it.
///
/// A minified bundle is one line of two hundred thousand characters, and a
/// model handed it has spent its context on nothing. The truncation is marked
/// so the model knows the line continued.
pub const MAX_LINE_LENGTH: usize = 2000;

/// The tools, and everything they share.
///
/// One value per session: the owner key is the session id, so two sessions in
/// one workspace never lend each other observations.
pub struct FsTools {
    fs: Arc<dyn FileSystem>,
    /// `None` composes the bare provider: unconditional mutation, no
    /// read-before-write rule. Named rather than defaulted, because a
    /// deployment that gets it by accident has lost a guard it never knew it
    /// had.
    observed: Option<Arc<ObservedState>>,
    owner: String,
}

impl FsTools {
    /// The composition a deployment wants: guarded mutation, keyed on one
    /// session.
    pub fn new(
        fs: Arc<dyn FileSystem>,
        observed: Arc<ObservedState>,
        owner: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            fs,
            observed: Some(observed),
            owner: owner.into(),
        })
    }

    /// The bare provider: every write and edit unconditional.
    ///
    /// Upstream's "without the observation-policy plugin" composition, kept
    /// because the guarded behaviour is only testable against the unguarded one
    /// and because a one-shot batch job legitimately has no session to key
    /// observations on.
    pub fn unobserved(fs: Arc<dyn FileSystem>, owner: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            fs,
            observed: None,
            owner: owner.into(),
        })
    }

    /// The filesystem these tools work through.
    pub fn filesystem(&self) -> &Arc<dyn FileSystem> {
        &self.fs
    }

    /// Register all seven into a registry.
    ///
    /// One call, so a deployment cannot compose five of them and wonder why
    /// the model keeps asking for the sixth.
    pub fn register(self: &Arc<Self>, registry: &mut ToolRegistry) {
        registry.register(Arc::new(ReadTool(Arc::clone(self))));
        registry.register(Arc::new(WriteTool(Arc::clone(self))));
        registry.register(Arc::new(EditTool(Arc::clone(self))));
        registry.register(Arc::new(ListTool(Arc::clone(self))));
        registry.register(Arc::new(GlobTool(Arc::clone(self))));
        registry.register(Arc::new(StatTool(Arc::clone(self))));
        registry.register(Arc::new(DeleteTool(Arc::clone(self))));
    }

    /// The names these tools register under, in canonical order. A deployment
    /// writing a `tools.order` needs them, and a test asserting the roster
    /// needs them to come from one place.
    pub const NAMES: &'static [&'static str] =
        &["delete", "edit", "glob", "list", "read", "stat", "write"];

    fn record(&self, target: &FsTarget, observation: Observation) {
        if let Some(state) = &self.observed {
            state.observe(&self.owner, target, observation);
        }
    }

    fn write_intent(&self, target: &FsTarget) -> WriteIntent {
        match &self.observed {
            Some(state) => state.write_intent(&self.owner, target),
            None => WriteIntent::Unconditional,
        }
    }

    fn edit_guard(&self, target: &FsTarget) -> Result<Option<crate::FsVersion>, FsError> {
        match &self.observed {
            Some(state) => state.edit_guard(&self.owner, target).map(Some),
            None => Ok(None),
        }
    }
}

/// How a filesystem refusal reaches the model.
///
/// The code first, then the sentence. A surface that routes on the class finds
/// it at a fixed position; a model reads straight past it to the part that says
/// what to do.
fn refused(error: &FsError) -> ToolOutcome {
    ToolOutcome::failed(format!("{}: {error}", error.code()))
}

/// Pull a required string argument, or say which one is missing.
///
/// The tool pipeline already checks arguments against the published schema, so
/// reaching one of these is a caller that dispatched without that check. It is
/// still answered as a bad-arguments error rather than by unwrapping, because a
/// tool that panics on a value the model sent takes a turn down with it.
fn required_str<'a>(tool: &str, args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| {
            ToolError::InvalidArguments(tool.into(), format!("`{key}` must be a non-empty string"))
        })
}

fn optional_usize(tool: &str, args: &Value, key: &str) -> Result<Option<usize>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|n| *n >= 1)
            .map(|n| Some(n as usize))
            .ok_or_else(|| {
                ToolError::InvalidArguments(
                    tool.into(),
                    format!("`{key}` must be a positive whole number"),
                )
            }),
    }
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// A path property, described the same way in every schema that takes one.
fn path_property(what: &str) -> Value {
    json!({
        "type": "string",
        "description": format!(
            "{what} Relative paths are taken against the workspace root; a path outside the \
             workspace is refused."
        ),
    })
}

macro_rules! fs_tool {
    ($name:ident, $tool:literal) => {
        pub struct $name(Arc<FsTools>);

        impl $name {
            /// Compose this tool alone, for a deployment that wants part of the
            /// set.
            pub fn new(tools: Arc<FsTools>) -> Arc<Self> {
                Arc::new(Self(tools))
            }

            /// The name this tool registers under.
            pub const NAME: &'static str = $tool;
        }
    };
}

fs_tool!(ReadTool, "read");
fs_tool!(WriteTool, "write");
fs_tool!(EditTool, "edit");
fs_tool!(ListTool, "list");
fs_tool!(GlobTool, "glob");
fs_tool!(StatTool, "stat");
fs_tool!(DeleteTool, "delete");

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Read a UTF-8 text file and return its lines, numbered. Use offset and \
                          limit to read a window of a long file."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": path_property("The file to read."),
                    "offset": {
                        "type": "integer",
                        "description": "1-based first line to return. Defaults to 1.",
                    },
                    "limit": {
                        "type": "integer",
                        "description": format!("How many lines to return, at most {READ_LIMIT}."),
                    },
                },
                "required": ["path"],
            }),
        }
    }

    /// A read changes nothing, so any number of reads may overlap.
    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let path = required_str(Self::NAME, arguments, "path")?;
        let offset = optional_usize(Self::NAME, arguments, "offset")?.unwrap_or(1);
        let limit = optional_usize(Self::NAME, arguments, "limit")?
            .unwrap_or(READ_LIMIT)
            .min(READ_LIMIT);

        let target = match self.0.fs.resolve(path) {
            Ok(target) => target,
            Err(error) => return Ok(refused(&error)),
        };
        match self.0.fs.read(&target) {
            Ok((text, version)) => {
                // Recorded before the window is rendered: what was observed is
                // the file, not the part of it that was shown.
                self.0.record(&target, Observation::Present(version));
                Ok(ToolOutcome::ok(window(
                    target.display(),
                    &text,
                    offset,
                    limit,
                )))
            }
            Err(error) => {
                // A read that found nothing there is an authoritative absence,
                // and recording it is what lets a later write create the file
                // rather than being told to read it first.
                if matches!(error, FsError::NotFound { .. }) {
                    self.0.record(&target, Observation::Absent);
                }
                Ok(refused(&error))
            }
        }
    }
}

#[async_trait::async_trait]
impl Tool for WriteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Create a file or replace its whole content. Read an existing file \
                          before writing it, or the write is refused."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": path_property("The file to write."),
                    "content": {
                        "type": "string",
                        "description": "The file's complete new content.",
                    },
                },
                "required": ["path", "content"],
            }),
        }
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let path = required_str(Self::NAME, arguments, "path")?;
        // Content may legitimately be empty - truncating a file is a write -
        // so it is read directly rather than through `required_str`.
        let content = arguments
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ToolError::InvalidArguments(Self::NAME.into(), "`content` must be a string".into())
            })?;

        let target = match self.0.fs.resolve(path) {
            Ok(target) => target,
            Err(error) => return Ok(refused(&error)),
        };
        let intent = self.0.write_intent(&target);
        match self.0.fs.write(&target, content, &intent) {
            Ok(outcome) => {
                // The file is now exactly what was written, at a known
                // version, so the session may write it again without reading
                // it back.
                self.0
                    .record(&target, Observation::Present(outcome.version.clone()));
                Ok(ToolOutcome::ok(format!(
                    "{} {} ({} lines, {} bytes)",
                    match outcome.operation {
                        crate::WriteOperation::Create => "created",
                        crate::WriteOperation::Update => "updated",
                    },
                    target.display(),
                    content.lines().count(),
                    content.len(),
                )))
            }
            Err(error) => Ok(refused(&error)),
        }
    }
}

#[async_trait::async_trait]
impl Tool for EditTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Replace literal text in a file that has already been read. The text \
                          must occur exactly once unless replace_all is set."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": path_property("The file to edit."),
                    "old_string": {
                        "type": "string",
                        "description": "The exact text to replace, with enough surrounding \
                                        context to occur only once.",
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement text. Empty deletes the matched text.",
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace every occurrence instead of requiring one.",
                    },
                },
                "required": ["path", "old_string", "new_string"],
            }),
        }
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let path = required_str(Self::NAME, arguments, "path")?;
        let old = required_str(Self::NAME, arguments, "old_string")?;
        let new = arguments
            .get("new_string")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ToolError::InvalidArguments(
                    Self::NAME.into(),
                    "`new_string` must be a string; use an empty one to delete the matched text"
                        .into(),
                )
            })?;
        let request = EditRequest {
            old: old.to_string(),
            new: new.to_string(),
            replace_all: flag(arguments, "replace_all"),
        };

        let target = match self.0.fs.resolve(path) {
            Ok(target) => target,
            Err(error) => return Ok(refused(&error)),
        };
        let guard = match self.0.edit_guard(&target) {
            Ok(guard) => guard,
            Err(error) => return Ok(refused(&error)),
        };
        match self.0.fs.edit(&target, &request, guard.as_ref()) {
            Ok(outcome) => {
                self.0
                    .record(&target, Observation::Present(outcome.version.clone()));
                Ok(ToolOutcome::ok(format!(
                    "edited {}: {} {} replaced",
                    target.display(),
                    outcome.replacements,
                    plural(outcome.replacements, "occurrence", "occurrences"),
                )))
            }
            Err(error) => Ok(refused(&error)),
        }
    }
}

#[async_trait::async_trait]
impl Tool for ListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "List the direct children of a directory, in name order.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": path_property("The directory to list. Defaults to the workspace root."),
                },
            }),
        }
    }

    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .unwrap_or(".");
        let target = match self.0.fs.resolve(path) {
            Ok(target) => target,
            Err(error) => return Ok(refused(&error)),
        };
        match self.0.fs.list(&target) {
            Ok(entries) if entries.is_empty() => {
                Ok(ToolOutcome::ok(format!("{} is empty", target.display())))
            }
            Ok(entries) => Ok(ToolOutcome::ok(rendered_listing(
                target.display(),
                &entries,
            ))),
            Err(error) => Ok(refused(&error)),
        }
    }
}

#[async_trait::async_trait]
impl Tool for GlobTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Find files by pattern. `*` matches inside one name, `**` matches any \
                          run of directories, `?` matches one character."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "The pattern, relative to `path`, such as `src/**/*.rs`.",
                    },
                    "path": path_property("The directory to search from. Defaults to the \
                                           workspace root."),
                },
                "required": ["pattern"],
            }),
        }
    }

    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let pattern = required_str(Self::NAME, arguments, "pattern")?;
        let base = arguments
            .get("path")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .unwrap_or(".");
        let target = match self.0.fs.resolve(base) {
            Ok(target) => target,
            Err(error) => return Ok(refused(&error)),
        };
        match self.0.fs.glob(&target, pattern) {
            Ok(found) if found.is_empty() => Ok(ToolOutcome::ok(format!(
                "no file under {} matches {pattern:?}",
                target.display()
            ))),
            Ok(found) => {
                let mut lines: Vec<String> =
                    found.iter().map(|t| t.display().to_string()).collect();
                if found.len() >= MAX_GLOB_MATCHES {
                    lines.push(format!(
                        "... stopped at {MAX_GLOB_MATCHES} matches; narrow the pattern to see \
                         the rest"
                    ));
                }
                Ok(ToolOutcome::ok(lines.join("\n")))
            }
            Err(error) => Ok(refused(&error)),
        }
    }
}

#[async_trait::async_trait]
impl Tool for StatTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Say whether a path exists and what is there, without reading it.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": path_property("The path to inspect.") },
                "required": ["path"],
            }),
        }
    }

    fn mode(&self, _arguments: &Value) -> ToolMode {
        ToolMode::Parallel
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let path = required_str(Self::NAME, arguments, "path")?;
        let target = match self.0.fs.resolve(path) {
            Ok(target) => target,
            Err(error) => return Ok(refused(&error)),
        };
        match self.0.fs.stat(&target) {
            // An absence a stat confirmed is as authoritative as a presence,
            // and recording it is what tells an edit "not found" instead of
            // "read it first".
            Ok(None) => {
                self.0.record(&target, Observation::Absent);
                Ok(ToolOutcome::ok(format!(
                    "{} does not exist",
                    target.display()
                )))
            }
            Ok(Some(info)) => {
                // A stat is an observation of presence, but it is deliberately
                // NOT one that authorizes an edit: the version is recorded so a
                // write can replace what is there, while an edit still needs
                // the content, which only a read has seen.
                self.0
                    .record(&target, Observation::Present(info.version.clone()));
                Ok(ToolOutcome::ok(match info.kind {
                    FileKind::Directory => format!("{} is a directory", target.display()),
                    FileKind::File => {
                        format!("{} is a file of {} bytes", target.display(), info.size)
                    }
                    other => format!("{} is a {}", target.display(), other.as_str()),
                }))
            }
            Err(error) => Ok(refused(&error)),
        }
    }
}

#[async_trait::async_trait]
impl Tool for DeleteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Delete a file, or a directory when recursive is set. This cannot be \
                          undone."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": path_property("The path to delete."),
                    "recursive": {
                        "type": "boolean",
                        "description": "Delete a directory and everything under it.",
                    },
                },
                "required": ["path"],
            }),
        }
    }

    /// Deleting is the one thing in this suite a session cannot take back.
    ///
    /// A write is recoverable in the sense that matters - the content it
    /// replaced is on the outcome, the fence keeps it inside the workspace, and
    /// the observation policy already refuses to overwrite what was not read.
    /// A delete leaves nothing to recover from, and a recursive one leaves a
    /// subtree missing. So this is where the gate goes, and the reason is
    /// written for the person answering rather than for a log: they need to
    /// know what disappears if they say yes.
    fn permission(&self, arguments: &Value) -> Permission {
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("an unnamed path");
        Permission::ask(if flag(arguments, "recursive") {
            format!("delete {path} and everything under it; this cannot be undone")
        } else {
            format!("delete {path}; this cannot be undone")
        })
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let path = required_str(Self::NAME, arguments, "path")?;
        let recursive = flag(arguments, "recursive");
        let target = match self.0.fs.resolve(path) {
            Ok(target) => target,
            Err(error) => return Ok(refused(&error)),
        };
        match self.0.fs.delete(&target, recursive) {
            Ok(deleted) => {
                // What is there now is nothing, and the session knows it: a
                // later write creates rather than being refused as blind.
                self.0.record(&target, Observation::Absent);
                Ok(ToolOutcome::ok(match deleted.kind {
                    FileKind::Directory => format!(
                        "deleted {} and {} {} under it",
                        target.display(),
                        deleted.entries - 1,
                        plural(deleted.entries - 1, "entry", "entries"),
                    ),
                    _ => format!("deleted {}", target.display()),
                }))
            }
            Err(error) => Ok(refused(&error)),
        }
    }
}

fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 {
        one
    } else {
        many
    }
}

/// Render a window of a file the way a model reads it best: a header saying
/// which part of what it is looking at, then numbered lines.
///
/// The header exists so a model that asked for lines 200-400 of a 4000-line
/// file knows it did not see the whole thing. Without it, a model reads a
/// window as a file and concludes the rest is not there.
fn window(display: &str, text: &str, offset: usize, limit: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = offset.saturating_sub(1);
    if start >= total {
        return format!(
            "{display} has {total} {}; line {offset} is past the end",
            plural(total, "line", "lines")
        );
    }
    let end = (start + limit).min(total);

    let mut out = String::new();
    if start == 0 && end == total {
        out.push_str(&format!(
            "{display} ({total} {})\n",
            plural(total, "line", "lines")
        ));
    } else {
        out.push_str(&format!(
            "{display} (lines {}-{} of {total})\n",
            start + 1,
            end
        ));
    }
    for (index, line) in lines[start..end].iter().enumerate() {
        out.push_str(&format!("{:>6}\t{}\n", start + index + 1, clipped(line)));
    }
    if end < total {
        out.push_str(&format!(
            "... {} more {} follow; read again from line {}\n",
            total - end,
            plural(total - end, "line", "lines"),
            end + 1
        ));
    }
    out
}

/// One line, bounded. A truncation is marked, because a model that cannot see
/// the mark reads the clipped text as the whole line and edits against it.
fn clipped(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_LENGTH {
        return line.to_string();
    }
    let kept: String = line.chars().take(MAX_LINE_LENGTH).collect();
    format!("{kept}... [line truncated at {MAX_LINE_LENGTH} characters]")
}

fn rendered_listing(display: &str, entries: &[DirEntry]) -> String {
    let mut out = format!(
        "{display} ({} {})\n",
        entries.len(),
        plural(entries.len(), "entry", "entries")
    );
    for entry in entries {
        match entry.kind {
            FileKind::Directory => out.push_str(&format!("{}/\n", entry.name)),
            FileKind::File => out.push_str(&format!("{} ({} bytes)\n", entry.name, entry.size)),
            other => out.push_str(&format!("{} ({})\n", entry.name, other.as_str())),
        }
    }
    out
}
