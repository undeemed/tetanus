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
    MAX_WINDOW_BYTES,
};

/// The default and maximum number of lines one `read` answers with.
pub const READ_LIMIT: usize = 2000;
/// The most characters of one line a read renders before it truncates it.
///
/// A minified bundle is one line of two hundred thousand characters, and a
/// model handed it has spent its context on nothing. The truncation is marked
/// so the model knows the line continued.
pub const MAX_LINE_LENGTH: usize = 2000;

/// The most matching lines one `search` answers with.
///
/// A search is worth having because it is cheaper than reading the files, and
/// a search that answers with two thousand lines has spent the saving. Past
/// this the answer says it stopped, which is what lets a model narrow the
/// pattern rather than believe it has seen everything.
pub const MAX_SEARCH_MATCHES: usize = 100;

/// The most files one `search` opens.
///
/// The bound is on files rather than on bytes because it is the one a caller
/// can act on: a pattern that reaches this was too broad, and the answer says
/// so with the number.
pub const MAX_SEARCH_FILES: usize = 2000;

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
    /// Where `read_image` puts a picture. [`crate::image::NoSink`] until a
    /// composition supplies one, so the tool explains its own absence rather
    /// than disappearing.
    images: crate::image::SharedSink,
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
            images: Arc::new(crate::image::NoSink),
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
            images: Arc::new(crate::image::NoSink),
        })
    }

    /// The filesystem these tools work through.
    pub fn filesystem(&self) -> &Arc<dyn FileSystem> {
        &self.fs
    }

    /// Register all nine into a registry.
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
        registry.register(Arc::new(SearchTool(Arc::clone(self))));
        registry.register(Arc::new(ReadImageTool(Arc::clone(self))));
    }

    /// The names these tools register under, in canonical order. A deployment
    /// writing a `tools.order` needs them, and a test asserting the roster
    /// needs them to come from one place.
    pub const NAMES: &'static [&'static str] = &[
        "delete",
        "edit",
        "glob",
        "list",
        "read",
        "read_image",
        "search",
        "stat",
        "write",
    ];

    /// Compose these tools with somewhere to put a picture.
    ///
    /// Taken after construction rather than as an argument, so a composition
    /// that has no store composes exactly as it did before this existed and
    /// one that has a store adds a line.
    pub fn with_images(self: Arc<Self>, sink: crate::image::SharedSink) -> Arc<Self> {
        Arc::new(Self {
            fs: Arc::clone(&self.fs),
            observed: self.observed.clone(),
            owner: self.owner.clone(),
            images: sink,
        })
    }

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
fs_tool!(SearchTool, "search");
fs_tool!(ReadImageTool, "read_image");
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
impl Tool for SearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Search file *contents* for a regular expression and return the matching \
                          lines with their file and line number. Use `glob` to find files by name \
                          instead. The pattern is a Rust regex: `\\bfn \\w+` finds function \
                          definitions. Binary files are skipped."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "The regular expression to look for in each line.",
                    },
                    "path": path_property("The directory to search under. Defaults to the \
                                           workspace root."),
                    "glob": {
                        "type": "string",
                        "description": "Only search files matching this name pattern, such as \
                                        `**/*.rs`. Defaults to every file under `path`.",
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "description": "Match case exactly. Defaults to false.",
                    },
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
        let matcher = match matcher_for(pattern, !flag(arguments, "case_sensitive")) {
            Ok(matcher) => matcher,
            Err(said) => return Ok(ToolOutcome::failed(said)),
        };
        let root = match self.0.fs.resolve(argument_or(arguments, "path", ".")) {
            Ok(target) => target,
            Err(error) => return Ok(refused(&error)),
        };
        let files = match self
            .0
            .fs
            .glob(&root, argument_or(arguments, "glob", "**/*"))
        {
            Ok(files) => files,
            Err(error) => return Ok(refused(&error)),
        };

        let found = self.scan(&files, &matcher);

        // A search does *not* count as observing a file for the
        // read-before-write rule, deliberately. It shows a model a handful of
        // lines out of a file it has otherwise never seen, and letting that
        // license a whole-file overwrite would turn the guard into a
        // formality - a model could grep for one word and replace everything.
        Ok(ToolOutcome::ok(render_search(
            &found,
            root.display(),
            pattern,
            files.len(),
        )))
    }
}

/// What one search found, and what it could not look at.
struct Found {
    lines: Vec<String>,
    files: usize,
    skipped: usize,
    scanned: usize,
    truncated: bool,
}

/// Whether a globbed entry is something this search can read.
enum Readable {
    Yes(String),
    No,
    NotAFile,
}

impl SearchTool {
    /// Read each file through the service and keep the lines that match.
    ///
    /// Through the service and not around it: the fence, the observation
    /// policy and the Landlock worker all live behind that seam, so a search
    /// that walked the directory itself would be a second answer to "may this
    /// be read".
    fn scan(&self, files: &[FsTarget], matcher: &regex::Regex) -> Found {
        let mut found = Found {
            lines: Vec::new(),
            files: 0,
            skipped: 0,
            scanned: 0,
            truncated: false,
        };
        for file in files.iter().take(MAX_SEARCH_FILES) {
            match self.readable(file) {
                Readable::Yes(text) => {
                    found.scanned += 1;
                    if keep_matches(&mut found, file, &text, matcher) {
                        found.files += 1;
                    }
                }
                // Counted and reported, never fatal and never silent: one
                // image in a source tree must not fail the call, and a search
                // that steps over a file quietly has answered "no matches"
                // about something it never looked at.
                Readable::No => found.skipped += 1,
                // Not a file at all. A glob answers with directories too, and
                // a directory is not a file this search could not read, so it
                // stays out of the count a reader acts on.
                Readable::NotAFile => {}
            }
            if found.truncated {
                break;
            }
        }
        found
    }

    fn readable(&self, file: &FsTarget) -> Readable {
        match self.0.fs.stat(file) {
            Ok(Some(info)) if info.kind == FileKind::File => match self.0.fs.read(file) {
                Ok((text, _)) => Readable::Yes(text),
                Err(_) => Readable::No,
            },
            Ok(_) => Readable::NotAFile,
            Err(_) => Readable::No,
        }
    }
}

/// Keep this file's matching lines, and say whether it matched at all.
fn keep_matches(found: &mut Found, file: &FsTarget, text: &str, matcher: &regex::Regex) -> bool {
    let mut hit = false;
    for (number, line) in text.lines().enumerate() {
        if !matcher.is_match(line) {
            continue;
        }
        hit = true;
        if found.lines.len() == MAX_SEARCH_MATCHES {
            found.truncated = true;
            break;
        }
        found.lines.push(format!(
            "{}:{}: {}",
            file.display(),
            number + 1,
            truncate_line(line)
        ));
    }
    hit
}

/// The pattern the model supplied, or the reason it is not one.
///
/// Case folding is an argument as well as a regex flag, because a model asking
/// for a case-insensitive search should not have to know that `(?i)` exists -
/// and `(?i)` in the pattern still works, since this only sets the default.
fn matcher_for(pattern: &str, fold_case: bool) -> Result<regex::Regex, String> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(fold_case)
        .size_limit(1 << 20)
        .build()
        // A bad pattern is the model's mistake and is answered as one, with
        // the regex crate's own explanation: it names the offset and what it
        // expected, which is what lets the next attempt differ from this one.
        .map_err(|error| {
            format!("FS_BAD_PATTERN: {pattern:?} is not a valid regular expression: {error}")
        })
}

fn argument_or<'a>(arguments: &'a Value, key: &str, fallback: &'a str) -> &'a str {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(fallback)
}

/// What the model reads: the count, then the lines, then why it is not more.
fn render_search(found: &Found, display: &str, pattern: &str, total_files: usize) -> String {
    if found.lines.is_empty() {
        return format!(
            "no line under {display} matches {pattern:?}{}",
            skipped_note(found.skipped)
        );
    }
    let mut out = format!(
        "{} matching {} in {} {}{}\n",
        found.lines.len(),
        plural(found.lines.len(), "line", "lines"),
        found.files,
        plural(found.files, "file", "files"),
        skipped_note(found.skipped)
    );
    out.push_str(&found.lines.join("\n"));
    if found.truncated {
        out.push_str(&format!(
            "\n... stopped at {MAX_SEARCH_MATCHES} matches; narrow the pattern or the glob to see \
             the rest"
        ));
    } else if total_files > MAX_SEARCH_FILES {
        out.push_str(&format!(
            "\n... searched the first {} files of {total_files}; narrow the glob to reach the rest",
            found.scanned
        ));
    }
    out
}

/// What a search says about the files it could not read.
///
/// Said rather than left out: a search that quietly skipped a file is a search
/// that answered "no matches" about something it never looked at.
fn skipped_note(skipped: usize) -> String {
    if skipped == 0 {
        String::new()
    } else {
        format!(
            " ({skipped} unreadable or non-text {} skipped)",
            plural(skipped, "file", "files")
        )
    }
}

/// One matching line, bounded the way a read's lines are bounded.
fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_LENGTH {
        return line.to_string();
    }
    let kept: String = line.chars().take(MAX_LINE_LENGTH).collect();
    format!("{kept} ... [line truncated]")
}

#[async_trait::async_trait]
impl Tool for ReadImageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Read a picture from the workspace and keep it as an attachment. Answers \
                          its id, media type, size and dimensions - never the bytes, so use this \
                          for what an image *is* rather than to quote it."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": path_property("The picture to read."),
                },
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
        // Bytes rather than text, through the same seam `read` uses: the fence
        // and the confined worker judge a picture exactly as they judge a
        // source file, and `read` itself would refuse this file twice over -
        // once for not being UTF-8 and once for being larger than the text cap.
        let (bytes, _) = match self.0.fs.read_bytes(&target, 0, MAX_WINDOW_BYTES) {
            Ok(read) => read,
            Err(error) => return Ok(refused(&error)),
        };
        if bytes.is_empty() {
            return Ok(ToolOutcome::failed(format!(
                "FS_NOT_FOUND: {} is empty, so there is no picture in it",
                target.display()
            )));
        }
        let name = std::path::Path::new(target.display())
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| target.display().to_string());

        // Reading a picture is not observing a file for the read-before-write
        // rule, for `search`'s reason turned around: the model never saw the
        // content at all, only a description of it, so it has less standing to
        // overwrite the file than a search gave it.
        match self.0.images.admit(&name, bytes) {
            Ok(stored) => Ok(ToolOutcome::ok(describe_image(&target, &stored))),
            Err(said) => Ok(ToolOutcome::failed(format!("FS_IMAGE_REFUSED: {said}"))),
        }
    }
}

/// What the model reads about a picture it will never see.
fn describe_image(target: &FsTarget, stored: &crate::image::Stored) -> String {
    let size = match stored.dimensions {
        Some((width, height)) => format!("{width}x{height}, "),
        // Said rather than omitted: a build that could not measure the header
        // and a picture that has no dimensions are different facts, and a
        // model deciding whether to ask a person to look wants to know which.
        None => "dimensions unread, ".to_string(),
    };
    format!(
        "{} kept as {} ({}{} bytes, {})",
        target.display(),
        stored.id,
        size,
        stored.bytes,
        stored.media_type
    )
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
