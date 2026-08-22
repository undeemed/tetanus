//! The one place that says which tools this build offers.
//!
//! **Why this is a crate.** Five tool crates landed - a filesystem service, a
//! command runner, the feature tools, an MCP client, web fetch and search - and
//! none of them can compose itself: the registry the binary uses lives in
//! `crates/cli`, which the presentation lane owns by
//! [`docs/interface-contract.md`](../../../docs/interface-contract.md) §4.7, so
//! no tool lane can add itself there. The assembly lives here instead, shaped
//! so that a landed crate is one line in [`sources`] and nothing else in the
//! workspace changes.
//!
//! **A crate existing is not the same as the binary offering its tools.** That
//! is the gap this crate closes, and it is the reason [`Assembly::build`] is
//! reachable from exactly two places in the binary - the catalogue and the
//! per-session registry - so `tetanus tools` cannot list a tool a turn cannot
//! dispatch, nor the other way round.
//!
//! **A tool comes from a source, and the source is named.** Grouping is not
//! decoration: it is what lets a deployment write `tools.sources: [fs]` instead
//! of naming fifteen tools, what lets a duplicate be reported as "these two
//! crates both offer `read`" rather than one silently winning, and what lets
//! `tetanus tools` say where a tool came from when a user asks why it is there.
//!
//! **A duplicate name is refused, not overwritten.**
//! [`tetanus_turn::tools::ToolRegistry::register`] keys by name and the last
//! registration wins, which is right for a registry and wrong for an assembly:
//! when two crates offer `read`, one of them is being silently dropped, and the
//! model is offered a schema belonging to a tool that is not the one that runs.
//! `read` is the collision to expect - `crates/fs` offers it and an MCP server
//! may too.
//!
//! **What a deployment enables, it enables by source.** An absent
//! `tools.sources` is every source this build ships; a list names exactly the
//! sources to use, and naming none is a harness with no tools - which is a
//! legitimate thing to want and a strange thing to arrive at by accident, so it
//! has to be written explicitly.
//!
//! **Reaching outside the machine stays opt-in.** `web` and `mcp` are composed
//! from the settings document and contribute nothing until it names them, which
//! is the posture `crates/web` already took for its own tools: a harness whose
//! first run in a sandbox quietly fetched a URL a model invented would be a
//! surprise nobody asked for. The sources are always *declared*, so
//! `tools.sources` and the roster read the same whether or not a deployment
//! turned them on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tetanus_config::{Config, ConfigError};
use tetanus_session::{SessionError, SessionEvent, SessionLog};
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::tools::{EchoTool, Tool, ToolRegistry};

/// The settings keys a deployment composes with.
pub mod key {
    /// Which sources to compose. Absent is all of them.
    pub const SOURCES: &str = "tools.sources";
    /// The filesystem mode the file tools are fenced by.
    pub const FS_MODE: &str = "fs.mode";
}

/// What the sources are built against.
///
/// Most tools that landed are not free-standing: the shell tools read the
/// turn's stop switch, the file tools key their read-before-write observations
/// on a session id, and the feature tools fold over that session's journal. So
/// the assembly takes a composition rather than a list of globals, and the
/// binary builds one per session.
///
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
    fn fs_mode(&self) -> tetanus_fs::FsMode {
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

/// One crate's worth of tools, under the name a deployment uses for it.
///
/// `Debug` reports the names rather than the tools: a tool is a trait object
/// with no useful rendering, and what a reader of a failed assembly needs is
/// which source held what.
pub struct Source {
    /// What a deployment writes in `tools.sources`, and what a duplicate
    /// report names. Stable: renaming one breaks a document that named it.
    pub name: &'static str,
    /// One line for `tetanus tools` and for the note that explains why a tool
    /// is on offer.
    pub description: &'static str,
    pub tools: Vec<Arc<dyn Tool>>,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("name", &self.name)
            .field("tools", &self.tool_names())
            .finish()
    }
}

impl std::fmt::Debug for Assembly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.sources.iter()).finish()
    }
}

impl Source {
    pub fn new(name: &'static str, description: &'static str, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            name,
            description,
            tools,
        }
    }

    /// A source from a crate that publishes `register(&mut ToolRegistry)` and
    /// no list.
    ///
    /// Most of them do, because registering is what a composer wanted before
    /// this crate existed. Draining a throwaway registry is what lets a crate
    /// land here without being asked for an accessor first - five lanes, five
    /// pull requests, all of them waiting on each other - and the assembly
    /// still sees each source's names separately, which is what the duplicate
    /// check needs.
    pub fn registered(
        name: &'static str,
        description: &'static str,
        build: impl FnOnce(&mut ToolRegistry),
    ) -> Self {
        let mut registry = ToolRegistry::new();
        build(&mut registry);
        let names: Vec<String> = registry.names().cloned().collect();
        let tools = names
            .iter()
            .filter_map(|name| registry.get(name))
            .collect::<Vec<_>>();
        Self::new(name, description, tools)
    }

    /// The names this source offers, in the order the registry will hold them.
    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.iter().map(|tool| tool.schema().name).collect();
        names.sort();
        names
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AssemblyError {
    /// Two sources offer one name. Refused rather than resolved, because
    /// either answer is wrong: the model would be offered one tool's schema
    /// and run the other's body, and nothing would say so.
    #[error(
        "the tool {name:?} is offered by both {first:?} and {second:?}. Two tools cannot share a \
         name: the model is offered one schema and would run the other body. Rename one, or \
         compose only one of the two sources"
    )]
    Duplicate {
        name: String,
        first: &'static str,
        second: &'static str,
    },
    #[error("no tool source is named {name:?}; this build ships {available}")]
    UnknownSource { name: String, available: String },
}

/// Every source this build ships, in the order a reader meets them.
///
/// **This function is the registration surface.** A tool crate that lands adds
/// exactly one entry here and changes nothing else in the workspace; the
/// catalogue and the per-session registry both come from here, so a tool added
/// here is a tool the model can call *and* a tool `tetanus tools` lists, and
/// those two cannot disagree.
///
/// A source that a deployment has not configured is declared and empty rather
/// than absent, so `tools.sources` names the same set on every host.
pub fn sources(cx: &Composition) -> Vec<Source> {
    vec![
        Source::new(
            "builtin",
            "The tools the engine ships with, which need no capability beyond the turn itself.",
            vec![Arc::new(EchoTool)],
        ),
        Source::registered(
            "exec",
            "Running commands and terminals, bounded in output and in time.",
            |registry| {
                // `register_or_explain`, not `register`: a host with no shell
                // still gets a `shell` tool that answers with the reason,
                // because a tool that quietly vanished looks to the model like
                // a build that never had one.
                tetanus_exec::tools::ShellTools::register_or_explain(
                    registry,
                    Arc::new(tetanus_exec::backend::Bash::new()),
                    tetanus_exec::shell::ShellConfig {
                        spill: spill_to(cx),
                        ..tetanus_exec::shell::ShellConfig::default()
                    },
                    tetanus_exec::session::SessionConfig::default(),
                    Arc::clone(&cx.interrupt),
                );
                register_terminals(registry, cx);
            },
        ),
        Source::registered(
            "fs",
            "Reading and changing files inside the workspace.",
            |registry| {
                // A backend this host refuses is a source with no tools rather
                // than a harness that will not start: every other source still
                // works, and `tetanus tools` shows the absence.
                match tetanus_fs::backend(cx.fs_mode(), &cx.workspace) {
                    Ok(fs) => tetanus_fs::FsTools::new(
                        fs,
                        Arc::new(tetanus_fs::ObservedState::new()),
                        cx.session_id.clone(),
                    )
                    .register(registry),
                    Err(refused) => {
                        tracing::warn!(%refused, "the file tools are unavailable on this host");
                    }
                }
            },
        ),
        Source::new(
            "features",
            "The task list, the standing goal, plan mode, feedback, skills and the workspace sketch.",
            features(cx),
        ),
        Source::new(
            "web",
            "Fetching a URL and searching the web.",
            // Empty unless `web.tools.*` turns them on. A document that
            // misconfigures them is reported where it is written, by the
            // binary that read it; here a refusal is no tools, because a
            // catalogue must not fail.
            tetanus_web::settings::tools(&cx.settings, std::env::var(tetanus_web::settings::KEY_ENV).ok().as_deref())
                .unwrap_or_default(),
        ),
        Source::new(
            "mcp",
            "Tools an MCP server offers, discovered at boot.",
            cx.mcp.clone(),
        ),
    ]
}

/// Where this session's oversized command output is kept, when it has a place
/// of its own to keep it.
///
/// A session with no journal on disk gets no store rather than a temporary
/// one: an artifact nobody can find later is a file nobody deletes either.
fn spill_to(cx: &Composition) -> Option<tetanus_exec::shell::SpillTo> {
    let artifacts = cx.artifacts.as_ref()?;
    Some(tetanus_exec::shell::SpillTo {
        store: Arc::new(tetanus_core::spill::SpillStore::at(
            artifacts.join("artifacts"),
        )),
        session: cx.session_id.clone(),
    })
}

/// The terminal family, where a host can have a terminal at all.
///
/// One registry of sessions per tool registry, and one tool registry per
/// session, so a session's terminals are its own twice over: by construction,
/// and by name. The name is what makes a foreign-session refusal mean
/// something - the registry compares owners exactly, and until a session had
/// an id to give it, every session called itself the same thing.
#[cfg(target_os = "linux")]
fn register_terminals(registry: &mut ToolRegistry, cx: &Composition) {
    use tetanus_exec::terminals::{Owner, Terminals};

    let terminals = match Terminals::with(
        tetanus_exec::terminal::TerminalConfig::default(),
        Arc::new(tetanus_exec::backend::Bash::new()),
    ) {
        Ok(terminals) => Arc::new(terminals),
        Err(refused) => {
            // Nothing here can fail on a host that has a bash, and the tools
            // are not worth failing a whole run over: a build with no terminal
            // family still has `shell` and `shell_run`.
            tracing::warn!(%refused, "the terminal tools are unavailable in this build");
            return;
        }
    };
    tetanus_exec::terminal_tools::TerminalTools::new(
        terminals,
        Owner::new(&cx.session_id),
        Arc::clone(&cx.interrupt),
    )
    .register(registry);
}

/// A host with no pseudo-terminals gets no terminal tools: they are the one
/// family that cannot be answered with an explanation, because opening a
/// terminal is the whole call.
#[cfg(not(target_os = "linux"))]
fn register_terminals(_registry: &mut ToolRegistry, _cx: &Composition) {}

/// The feature tools, each composed with the journal it folds over.
///
/// A function rather than a line because there are seven of them from six
/// modules, and a `vec!` of seven constructor calls inside `sources` would bury
/// the one line per crate that makes that list readable.
fn features(cx: &Composition) -> Vec<Arc<dyn Tool>> {
    let roots = tetanus_features::skill::default_roots(&cx.workspace, cx.home.as_deref(), &[]);
    let roster = Arc::new(tetanus_features::skill::discover(&roots));
    vec![
        tetanus_features::todo::TodoWriteTool::new(
            Arc::clone(&cx.log),
            // `Parallelism` has no default on purpose - `crates/features` says
            // the composer states it - and this is the composer. One active
            // task is the discipline that makes the list mean anything for
            // sequential work, which is what a turn is.
            tetanus_features::todo::Parallelism::SingleActive,
        ),
        tetanus_features::goal::GoalReadTool::new(Arc::clone(&cx.log)),
        tetanus_features::goal::GoalWriteTool::new(Arc::clone(&cx.log)),
        tetanus_features::plan::ExitPlanModeTool::new(Arc::clone(&cx.log)),
        tetanus_features::feedback::FeedbackTool::new(Arc::clone(&cx.log)),
        tetanus_features::skill::SkillTool::new(roster),
        tetanus_features::workspace::WorkspaceInfoTool::new(cx.workspace.clone()),
    ]
}

/// The sources this build ships, composed and checked.
pub struct Assembly {
    sources: Vec<Source>,
}

impl Assembly {
    /// Everything this build ships, built against one composition.
    pub fn stock(cx: &Composition) -> Self {
        Self {
            sources: sources(cx),
        }
    }

    /// An assembly of exactly the sources given, for a composer that is not
    /// taking the shipped set - a test, or an embedder with its own tools.
    pub fn of(sources: Vec<Source>) -> Self {
        Self { sources }
    }

    /// Add one source.
    #[must_use]
    pub fn with(mut self, source: Source) -> Self {
        self.sources.push(source);
        self
    }

    /// Keep only the named sources, in the order this build declares them
    /// rather than the order they were named.
    ///
    /// The declared order is what makes an assembly reproducible: two
    /// deployments naming the same sources in different orders get the same
    /// registry, and a document is not a place to express precedence that
    /// nothing reads. `tools.order` is where the order the model sees is set.
    pub fn only<I, S>(mut self, names: I) -> Result<Self, AssemblyError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let wanted: Vec<String> = names.into_iter().map(|n| n.as_ref().to_string()).collect();
        for name in &wanted {
            if !self.sources.iter().any(|source| source.name == name) {
                return Err(AssemblyError::UnknownSource {
                    name: name.clone(),
                    available: self.names().join(", "),
                });
            }
        }
        self.sources
            .retain(|source| wanted.iter().any(|name| name == source.name));
        Ok(self)
    }

    /// Apply `tools.sources` from a settings document.
    ///
    /// An absent key leaves every source composed; a list keeps exactly what it
    /// names, and an empty list keeps none. A deployment that wants a harness
    /// with no tools has to write that down, which is the difference between
    /// choosing it and arriving at it.
    pub fn configured(self, settings: &Config) -> Result<Self, ConfigError> {
        let Some(resolved) = settings.get(key::SOURCES) else {
            return Ok(self);
        };
        let Value::Array(items) = &resolved.value else {
            return Err(bad(&resolved.value));
        };
        let mut names = Vec::with_capacity(items.len());
        for item in items {
            match item.as_str() {
                Some(name) => names.push(name.to_string()),
                None => return Err(bad(&resolved.value)),
            }
        }
        // Reported as the one name that is wrong, not as the whole list: a
        // reader fixing a document needs the word to change, and "must be a
        // list of names, not <the whole error sentence>" is what a nested
        // message reads like.
        self.only(names).map_err(|error| match error {
            AssemblyError::UnknownSource { name, available } => ConfigError::BadValue {
                key: key::SOURCES.to_string(),
                expected: format!("a source this build ships: {available}"),
                found: name,
            },
            clash => ConfigError::BadValue {
                key: key::SOURCES.to_string(),
                expected: "sources whose tools have distinct names".to_string(),
                found: clash.to_string(),
            },
        })
    }

    /// The source names, in declaration order.
    pub fn names(&self) -> Vec<&'static str> {
        self.sources.iter().map(|source| source.name).collect()
    }

    /// What each source contributes: its name, its line, and its tools.
    ///
    /// Published because "why is this tool on offer" is a question a user asks
    /// of a harness with twenty of them, and the answer has to come from the
    /// same place the tools do.
    pub fn roster(&self) -> Vec<(&'static str, &'static str, Vec<String>)> {
        self.sources
            .iter()
            .map(|source| (source.name, source.description, source.tool_names()))
            .collect()
    }

    /// Which source offers a given tool, if any.
    pub fn source_of(&self, tool: &str) -> Option<&'static str> {
        self.sources
            .iter()
            .find(|source| source.tool_names().iter().any(|name| name == tool))
            .map(|source| source.name)
    }

    /// Compose the registry, refusing a name two sources share.
    ///
    /// The check is here rather than in the registry because the registry is
    /// right to key by name - one name, one tool - and what is wrong is a
    /// *composition* that produced two. Refusing at the seam that built it
    /// names both crates, which is the only form of the message anybody can
    /// act on.
    pub fn build(self) -> Result<ToolRegistry, AssemblyError> {
        let mut owner: BTreeMap<String, &'static str> = BTreeMap::new();
        let mut registry = ToolRegistry::new();
        for source in &self.sources {
            for tool in &source.tools {
                let name = tool.schema().name;
                if let Some(first) = owner.get(&name) {
                    return Err(AssemblyError::Duplicate {
                        name,
                        first,
                        second: source.name,
                    });
                }
                owner.insert(name, source.name);
                registry.register(Arc::clone(tool));
            }
        }
        Ok(registry)
    }
}

fn bad(found: &Value) -> ConfigError {
    ConfigError::BadValue {
        key: key::SOURCES.to_string(),
        expected: "a list of source names".to_string(),
        found: found.to_string(),
    }
}

/// The registry this build offers for one composition, with the document
/// applied.
///
/// The one function the binary calls, for both the catalogue and each
/// session's registry, so what `tetanus tools` lists and what a turn can
/// dispatch are the same list rather than two expressions that agree today.
pub fn registry(cx: &Composition) -> Result<ToolRegistry, ConfigError> {
    Assembly::stock(cx)
        .configured(&cx.settings)?
        .build()
        .map_err(|clash| ConfigError::BadValue {
            key: key::SOURCES.to_string(),
            expected: "sources whose tools have distinct names".to_string(),
            found: clash.to_string(),
        })
}

/// Check that `tools.sources` names sources this build ships.
///
/// For a caller that will build a registry per session: a document is wrong
/// once, and it should be reported once, at boot, by the surface that read it,
/// not on the first prompt from inside a closure with nowhere to return a
/// failure to. The source *names* do not depend on what a source is composed
/// with, so a catalogue composition answers for every session.
pub fn check(settings: &Arc<Config>) -> Result<(), ConfigError> {
    let cx = Composition::catalogue().settings(Arc::clone(settings));
    registry(&cx).map(|_| ())
}

/// The shipped registry for a composition that needs no session.
///
/// It cannot fail on a duplicate: the shipped sources are this build's own, so
/// a clash among them is a mistake in [`sources`] rather than anything a
/// deployment did, and TC-TOOLSET-1 catches it before it can ship. A bad
/// `tools.sources` still refuses, which is why this takes no document.
pub fn stock_registry() -> ToolRegistry {
    Assembly::stock(&Composition::catalogue())
        .build()
        .expect("the shipped sources are this build's own; TC-TOOLSET-1 holds them unique")
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
