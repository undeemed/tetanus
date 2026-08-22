//! Which tools this binary offers, and how one registry is built.
//!
//! It is a module rather than a few functions in `main.rs` for a measured
//! reason: the entry point is the file every other file already reaches
//! through, and the structural gate charges for growing it - the terminal
//! family cost 432 points as forty-five lines there and nothing as a module of
//! its own. `CONTRIBUTING.md` says so under "the structural gate"; this is
//! that advice taken.

use std::sync::Arc;

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
        // is one nothing will ever throw.
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

/// The one registry, so what is listed and what is callable are one thing.
///
/// The shell tools hold work outside this process, so they are built against
/// the interrupt the turn they serve will run under: stopping a turn has to
/// reach the command it started, not just the step boundary. A host with no
/// shell still gets a `shell` tool - one that answers every call with the
/// deployment fault, because a tool that quietly vanished would look to the
/// model like a build that never had one.
pub fn registry(interrupt: &Arc<Interrupt>) -> ToolRegistry {
    let mut registry = ToolRegistry::new().with(Arc::new(EchoTool));
    tetanus_exec::tools::ShellTools::register_or_explain(
        &mut registry,
        Arc::new(tetanus_exec::backend::Bash::new()),
        tetanus_exec::shell::ShellConfig::default(),
        tetanus_exec::session::SessionConfig::default(),
        Arc::clone(interrupt),
    );
    register_terminals(&mut registry, interrupt);
    registry
}

/// The terminal family, where a host can have a terminal at all.
///
/// One registry of sessions per tool registry, and one tool registry per
/// session, so a session's terminals are its own: the owner below is a name
/// for that boundary rather than an identity, because tetanus sessions do not
/// have one yet.
#[cfg(target_os = "linux")]
fn register_terminals(registry: &mut ToolRegistry, interrupt: &Arc<Interrupt>) {
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
        Owner::new("session"),
        Arc::clone(interrupt),
    )
    .register(registry);
}

/// A host with no pseudo-terminals gets no terminal tools: they are the one
/// family that cannot be answered with an explanation, because opening a
/// terminal is the whole call.
#[cfg(not(target_os = "linux"))]
fn register_terminals(_registry: &mut ToolRegistry, _interrupt: &Arc<Interrupt>) {}

/// The tools one session runs with, for the engine that serves many sessions
/// at once. Each session gets its own, because each has its own interrupt.
pub fn session_tools() -> tetanus_engine::agent::SessionTools {
    Arc::new(|interrupt: &Arc<Interrupt>| Arc::new(registry(interrupt)))
}
