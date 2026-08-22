//! What the sources are built against, and the workspace root they use.
//!
//! Split from the assembly because it answers a different question: the
//! assembly decides *which* tools this build offers, and this decides what
//! each one is wired to. They change for different reasons - a landed crate
//! touches `sources`, a tool that needs something new from its session
//! touches this.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tetanus_config::Config;
use tetanus_session::{SessionError, SessionEvent, SessionLog};
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::tools::Tool;

use crate::key;

/// The defaults describe the one composition that needs no session: a
/// catalogue, which reads schemas and runs nothing.
pub struct Composition {
    /// The stop switch this session's tools read, so `agent.interrupt` reaches
    /// a command a tool started rather than only the step boundary.
    pub interrupt: Arc<Interrupt>,
    /// The journal the journal-backed tools fold over.
    pub log: Arc<dyn SessionLog>,
    /// Who read-before-write observations are keyed on.
    pub session_id: String,
    /// The directory the file tools are fenced to and the workspace tools
    /// describe.
    pub workspace: PathBuf,
    /// Where this session's durable artifacts already live - the directory of
    /// its journal - for a tool that has to put something on disk. `None` is a
    /// session with no file behind it, and a tool that needs one keeps
    /// nothing: an artifact nobody can find later is a file nobody deletes
    /// either.
    pub artifacts: Option<PathBuf>,
    /// The harness home, for the skill roots that live under it.
    pub home: Option<PathBuf>,
    /// The resolved settings document. `web`, `mcp` and the filesystem mode
    /// are read from it.
    pub settings: Arc<Config>,
    /// Tools an MCP server advertised, already connected.
    ///
    /// A handshake is asynchronous and a composition is not, so the binary
    /// connects its declared servers once at boot and hands the result in.
    pub mcp: Vec<Arc<dyn Tool>>,
}

impl Default for Composition {
    fn default() -> Self {
        Self::catalogue()
    }
}

impl Composition {
    /// The composition a listing is built against: no session, so a stop
    /// switch nothing will throw and a journal nothing will write.
    ///
    /// Every tool is constructed, because a catalogue that constructed a
    /// different set from the one a turn gets is the drift this crate exists
    /// to prevent. None is *called*: `tetanus tools` reads schemas.
    pub fn catalogue() -> Self {
        Self {
            interrupt: Interrupt::new(),
            log: Arc::new(UnusedLog),
            session_id: "catalogue".to_string(),
            workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            artifacts: None,
            home: None,
            settings: Arc::new(Config::default()),
            mcp: Vec::new(),
        }
    }

    /// The composition one session's turns run against.
    pub fn for_session(
        interrupt: Arc<Interrupt>,
        log: Arc<dyn SessionLog>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            interrupt,
            session_id: session_id.into(),
            log,
            ..Self::catalogue()
        }
    }

    /// Read the document these sources are configured from.
    #[must_use]
    pub fn settings(mut self, settings: Arc<Config>) -> Self {
        self.settings = settings;
        self
    }

    /// Fence the file tools to this directory, and describe it to the
    /// workspace tools.
    #[must_use]
    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = workspace.into();
        self
    }

    /// The harness home, for the skill roots under it.
    #[must_use]
    pub fn home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home;
        self
    }

    /// Where this session already keeps things on disk.
    ///
    /// The directory is made absolute here, because what is built from it is a
    /// locator a *model* reads and a presentation may open. A relative path
    /// resolves against whatever directory the reader happens to be in, which
    /// for anything but this process is the wrong one - and a journal named
    /// `j.jsonl` has `""` for a parent, which is not a directory at all.
    #[must_use]
    pub fn artifacts(mut self, artifacts: Option<&Path>) -> Self {
        self.artifacts = artifacts.map(|dir| {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())
        });
        self
    }

    /// The tools already-connected MCP servers advertise.
    #[must_use]
    pub fn mcp(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.mcp = tools;
        self
    }

    /// The filesystem mode the document names, or the fenced default.
    ///
    /// A mode this build does not know is the document's mistake and is
    /// reported by `crates/fs` when it is read; here an unreadable value falls
    /// back to the fence rather than to full access, because the failure mode
    /// of guessing wrong has to be the safe one.
    pub(crate) fn fs_mode(&self) -> tetanus_fs::FsMode {
        self.settings
            .get(key::FS_MODE)
            .and_then(|resolved| resolved.value.as_str().map(str::to_string))
            .and_then(|name| tetanus_fs::FsMode::parse(&name).ok())
            .unwrap_or_default()
    }
}

/// A journal for a composition that will never append to one.
///
/// [`Composition::catalogue`] builds every tool so that what is listed is what
/// runs, and a listing has no session to write to. Appending is a *failure*
/// rather than a silent discard: if one of these ever reaches a running tool,
/// the tool says so instead of reporting success for work that went nowhere.
struct UnusedLog;

impl SessionLog for UnusedLog {
    fn id(&self) -> &str {
        "catalogue"
    }

    fn append(&self, ty: &str, _data: Value) -> Result<SessionEvent, SessionError> {
        Err(SessionError::Store(format!(
            "{ty:?} was appended to a catalogue composition, which has no session to write to"
        )))
    }

    fn append_with_sources(
        &self,
        ty: &str,
        data: Value,
        _sources: Vec<u64>,
    ) -> Result<SessionEvent, SessionError> {
        self.append(ty, data)
    }

    fn events(&self) -> Vec<SessionEvent> {
        Vec::new()
    }

    fn flush(&self) -> Result<(), SessionError> {
        Ok(())
    }
}

/// The workspace root a composition should be fenced to, given what the user
/// asked for.
///
/// Published because the binary and its cases have to agree on it, and
/// "current directory unless told otherwise" is the kind of default that grows
/// a second, slightly different copy the moment it is written twice.
pub fn workspace_root(named: Option<&Path>) -> PathBuf {
    named
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}
