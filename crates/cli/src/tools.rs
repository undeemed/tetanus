//! Which tools this binary offers, and how one registry is built.
//!
//! It is a module rather than a few functions in `main.rs` for a measured
//! reason: the entry point is the file every other file already reaches
//! through, and the structural gate charges for growing it - the terminal
//! family cost 432 points as forty-five lines there and nothing as a module of
//! its own. `CONTRIBUTING.md` says so under "the structural gate"; this is
//! that advice taken.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tetanus_engine::agent::ToolScope;
use tetanus_protocol::methods::ToolCatalogResult;
use tetanus_protocol::types as protocol;
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::tools::{EchoTool, ToolRegistry};

/// The tools an agent may call. Built from the registry a turn is booted with,
/// so `tetanus tools` cannot list a tool a run does not have. It answers
/// `catalog.tools`.
pub fn catalog() -> ToolCatalogResult {
    ToolCatalogResult {
        // A catalog only reads schemas, so the switch it builds them against
        // is one nothing will ever throw, and the session it names is nobody's
        // - no tool is called.
        tools: registry(&Interrupt::new())
            .schemas()
            .into_iter()
            .map(|schema| protocol::ToolDescriptor {
                name: schema.name,
                description: schema.description,
                parameters: schema.parameters,
            })
            .collect(),
    }
}

/// Whose tools these are: the session that will call them, and where its
/// durable artifacts live.
///
/// Two things in the registry need it. A terminal belongs to the session that
/// opened it, and the registry that holds terminals compares owners exactly,
/// so the owner has to be a real name rather than a constant. And a command
/// whose output outgrows its capture bound has the rest of that output kept on
/// disk, which belongs beside the session's journal - the place a reader is
/// already looking - rather than in a directory this binary invented.
#[derive(Debug, Clone, Default)]
pub struct Whose {
    pub session: String,
    pub artifacts: Option<PathBuf>,
}

impl Whose {
    /// A session by id, keeping its artifacts wherever its journal is.
    ///
    /// The directory is made absolute here, because what is built from it is a
    /// locator a *model* reads and a presentation may open. A relative path
    /// resolves against whatever directory the reader happens to be in, which
    /// for anything but this process is the wrong one - and a journal named
    /// `j.jsonl` has `""` for a parent, which is not a directory at all.
    pub fn session(id: &str, artifacts: Option<&Path>) -> Self {
        let artifacts = artifacts.map(|dir| {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())
        });
        Self {
            session: id.to_string(),
            artifacts,
        }
    }
}

/// The one registry, so what is listed and what is callable are one thing.
///
/// The shell tools hold work outside this process, so they are built against
/// the interrupt the turn they serve will run under: stopping a turn has to
/// reach the command it started, not just the step boundary. A host with no
/// shell still gets a `shell` tool - one that answers every call with the
/// deployment fault, because a tool that quietly vanished would look to the
/// model like a build that never had one.
pub fn registry(interrupt: &Arc<Interrupt>) -> ToolRegistry {
    registry_for(&Whose::default(), interrupt)
}

/// [`registry`], for one named session.
pub fn registry_for(whose: &Whose, interrupt: &Arc<Interrupt>) -> ToolRegistry {
    let mut registry = ToolRegistry::new().with(Arc::new(EchoTool));
    tetanus_exec::tools::ShellTools::register_or_explain(
        &mut registry,
        Arc::new(tetanus_exec::backend::Bash::new()),
        tetanus_exec::shell::ShellConfig {
            spill: spill_to(whose),
            ..tetanus_exec::shell::ShellConfig::default()
        },
        tetanus_exec::session::SessionConfig::default(),
        Arc::clone(interrupt),
    );
    register_terminals(&mut registry, whose, interrupt);
    registry
}

/// Where this session's oversized command output is kept, when it has a place
/// of its own to keep it.
///
/// A session with no journal on disk gets no store rather than a temporary
/// one: an artifact nobody can find later is a file nobody deletes either.
fn spill_to(whose: &Whose) -> Option<tetanus_exec::shell::SpillTo> {
    let artifacts = whose.artifacts.as_ref()?;
    Some(tetanus_exec::shell::SpillTo {
        store: Arc::new(tetanus_core::spill::SpillStore::at(
            artifacts.join("artifacts"),
        )),
        session: whose.session.clone(),
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
fn register_terminals(registry: &mut ToolRegistry, whose: &Whose, interrupt: &Arc<Interrupt>) {
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
            eprintln!("the terminal tools are unavailable in this build: {refused}");
            return;
        }
    };
    tetanus_exec::terminal_tools::TerminalTools::new(
        terminals,
        Owner::new(&whose.session),
        Arc::clone(interrupt),
    )
    .register(registry);
}

/// A host with no pseudo-terminals gets no terminal tools: they are the one
/// family that cannot be answered with an explanation, because opening a
/// terminal is the whole call.
#[cfg(not(target_os = "linux"))]
fn register_terminals(_registry: &mut ToolRegistry, _whose: &Whose, _interrupt: &Arc<Interrupt>) {}

/// The tools one session runs with, for the engine that serves many sessions
/// at once. Each session gets its own, because each has its own interrupt.
pub fn session_tools() -> tetanus_engine::agent::SessionTools {
    Arc::new(|scope: &ToolScope<'_>, interrupt: &Arc<Interrupt>| {
        Arc::new(registry_for(
            &Whose::session(scope.session_id, scope.artifacts),
            interrupt,
        ))
    })
}
