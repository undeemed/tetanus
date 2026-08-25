//! Finding a deployment's hook configuration, and composing a bridge from it.
//!
//! Everything else in this crate is given its hooks. This is the part that
//! reads them off disk, which is the last thing standing between a bridged
//! turn and a deployment that can actually switch hooks on.
//!
//! # Reading is separate from parsing, and both are separate from composing
//!
//! Three failures live here and a deployment does something different about
//! each: the file is not there, the file is not JSON, or the file is JSON that
//! does not describe hooks. [`LoadError`] keeps them apart, because "no hooks
//! configured" and "your hooks file has a typo in it" must not read the same -
//! the first is the ordinary case and the second is a silent loss of every
//! guard a deployment thought it had.
//!
//! # An absent file is not an error
//!
//! Most deployments configure no hooks. Asking for a path that is not there
//! answers an empty configuration, so composing a bridge is unconditional and
//! a deployment does not have to ask whether it has hooks before wiring them.
//! A path that *is* there and cannot be read is a different matter and is
//! reported: somebody wrote that file on purpose.
//!
//! # What a hook process is told about where it is
//!
//! Claude Code exports `CLAUDE_PROJECT_DIR` to every hook, and unmodified
//! hooks in the wild use it to find project-relative files. A bridge that
//! dropped it would run those hooks successfully and have them look in the
//! wrong place, which is worse than not running them. It is also the value
//! substituted into `${CLAUDE_PROJECT_DIR}` in a command, so the two cannot
//! disagree: [`DiscoveredHooks`] resolves it once and uses it for both.
//!
//! Parity: upstream `packages/hooks/hooks-claude-code/src/index.ts` and
//! `packages/hooks/hooks-codex/src/index.ts`, the `configPath` half of `apply`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::claude_code::{parse_claude_code_config, SubstitutionVars};
use crate::codex::parse_codex_config;
use crate::events::HookDialect;
use crate::MatcherGroup;

/// The environment variable Claude Code exports to every hook process.
pub const CLAUDE_PROJECT_DIR: &str = "CLAUDE_PROJECT_DIR";

/// Why a hook configuration could not be loaded.
///
/// Three variants and not one string, because a deployment's next move
/// differs: a missing file may be intentional, unreadable JSON is a typo, and
/// a shape this harness does not understand is a version mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// The file exists but could not be read.
    Unreadable { path: String, reason: String },
    /// The file was read but is not JSON.
    NotJson { path: String, reason: String },
    /// The file is JSON but does not describe hooks this harness can run.
    NotHooks { path: String, reason: String },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Each sentence names the file, because a deployment may configure one
        // per dialect and a message that does not say which is a message that
        // sends somebody to the wrong file.
        match self {
            Self::Unreadable { path, reason } => {
                write!(
                    f,
                    "the hook configuration at {path} could not be read: {reason}"
                )
            }
            Self::NotJson { path, reason } => {
                write!(f, "the hook configuration at {path} is not JSON: {reason}")
            }
            Self::NotHooks { path, reason } => {
                write!(
                    f,
                    "the hook configuration at {path} could not be understood: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// One dialect's hooks, as found on disk.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiscoveredHooks {
    /// The groups for each point, by the dialect's own event name.
    pub by_event: Vec<(String, Vec<MatcherGroup>)>,
    /// Hooks the file declared that this harness will not run, so a caller can
    /// say so rather than leaving a deployment to wonder why nothing fires.
    pub skipped: Vec<String>,
    /// The project directory a hook process is told about, when one is known.
    pub project_dir: Option<String>,
}

impl DiscoveredHooks {
    /// The groups configured for one point, empty when the file named none.
    pub fn event(&self, event: &str) -> Vec<MatcherGroup> {
        self.by_event
            .iter()
            .find(|(name, _)| name == event)
            .map(|(_, groups)| groups.clone())
            .unwrap_or_default()
    }

    /// Whether any point has a hook. A deployment with none pays for nothing.
    pub fn is_empty(&self) -> bool {
        self.by_event.iter().all(|(_, groups)| groups.is_empty())
    }

    /// The environment a hook process is started with.
    ///
    /// Only the variables this bridge is responsible for. The executor decides
    /// what else a child inherits, which is `crates/exec`'s policy and not
    /// this crate's to duplicate.
    pub fn env(&self) -> Vec<(String, String)> {
        match &self.project_dir {
            Some(dir) => vec![(CLAUDE_PROJECT_DIR.to_owned(), dir.clone())],
            None => Vec::new(),
        }
    }
}

/// Where a bridge looks, and what it substitutes into the commands it finds.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    /// The configuration file. A `hooks.json`, or a settings file whose
    /// `hooks` key holds the same map - both parsers accept either, so a
    /// deployment does not have to say which it wrote.
    pub path: PathBuf,
    /// Replaces `${CLAUDE_PLUGIN_ROOT}` in a command.
    pub plugin_root: Option<String>,
    /// Replaces `${CLAUDE_PROJECT_DIR}` in a command, and is exported to the
    /// hook process as `CLAUDE_PROJECT_DIR`.
    pub project_dir: Option<String>,
}

impl Discovery {
    /// Look at `path`, substituting nothing.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            plugin_root: None,
            project_dir: None,
        }
    }

    /// The project directory this discovery reports and substitutes.
    pub fn in_project(mut self, dir: impl Into<String>) -> Self {
        self.project_dir = Some(dir.into());
        self
    }

    /// The plugin root substituted into `${CLAUDE_PLUGIN_ROOT}`.
    pub fn with_plugin_root(mut self, root: impl Into<String>) -> Self {
        self.plugin_root = Some(root.into());
        self
    }

    /// Read and parse this dialect's configuration.
    ///
    /// An absent file answers an empty configuration rather than an error: a
    /// deployment that configured no hooks is the ordinary case, and making
    /// the caller ask first would mean every composition site repeating the
    /// same check. A file that exists and will not read is reported, because
    /// somebody wrote it deliberately and silence would lose every guard they
    /// thought they had.
    pub fn load(&self, dialect: HookDialect) -> Result<DiscoveredHooks, LoadError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DiscoveredHooks {
                    project_dir: self.project_dir.clone(),
                    ..Default::default()
                })
            }
            Err(error) => {
                return Err(LoadError::Unreadable {
                    path: self.display(),
                    reason: error.to_string(),
                })
            }
        };
        self.parse(dialect, &text)
    }

    /// Parse configuration text that has already been read.
    ///
    /// Separate from [`Discovery::load`] so a deployment holding its settings
    /// in something other than a file - a database, an environment variable -
    /// reaches the same parser rather than writing a temporary file to use
    /// this crate.
    pub fn parse(&self, dialect: HookDialect, text: &str) -> Result<DiscoveredHooks, LoadError> {
        let raw: Value = serde_json::from_str(text).map_err(|error| LoadError::NotJson {
            path: self.display(),
            reason: error.to_string(),
        })?;

        let (by_event, skipped) = match dialect {
            HookDialect::ClaudeCode => {
                let vars = SubstitutionVars {
                    plugin_root: self.plugin_root.clone(),
                    project_dir: self.project_dir.clone(),
                };
                let parsed =
                    parse_claude_code_config(&raw, &vars).map_err(|error| LoadError::NotHooks {
                        path: self.display(),
                        reason: error.to_string(),
                    })?;
                let skipped = parsed
                    .skipped
                    .iter()
                    .map(|hook| format!("{}: {}", hook.event, hook.ty))
                    .collect();
                (parsed.config, skipped)
            }
            HookDialect::Codex => {
                let parsed = parse_codex_config(&raw).map_err(|error| LoadError::NotHooks {
                    path: self.display(),
                    reason: error.to_string(),
                })?;
                let skipped = parsed
                    .skipped
                    .iter()
                    .map(|hook| format!("{}: {}", hook.event, hook.reason))
                    .collect();
                (parsed.config, skipped)
            }
        };

        Ok(DiscoveredHooks {
            by_event,
            skipped,
            project_dir: self.project_dir.clone(),
        })
    }

    fn display(&self) -> String {
        self.path.display().to_string()
    }
}

/// The path a deployment most likely means, given a workspace.
///
/// A convenience and not a search: this returns one candidate rather than
/// hunting a directory tree, because a bridge that silently picked up a file
/// nobody pointed it at is a bridge that runs programs nobody authorised. A
/// deployment names its path; this only spells the conventional one.
pub fn conventional_path(workspace: &Path, dialect: HookDialect) -> PathBuf {
    match dialect {
        HookDialect::ClaudeCode => workspace.join(".claude").join("settings.json"),
        HookDialect::Codex => workspace.join(".codex").join("hooks.json"),
    }
}
