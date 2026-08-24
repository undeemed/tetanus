//! Which tools this binary offers, and how one registry is built.
//!
//! It is a module rather than a few functions in `main.rs` for a measured
//! reason: the entry point is the file every other file already reaches
//! through, and the structural gate charges for growing it - the terminal
//! family cost 432 points as forty-five lines there and nothing as a module of
//! its own. `CONTRIBUTING.md` says so under "the structural gate"; this is
//! that advice taken.
//!
//! What each function does is now one call into [`tetanus_toolset`], which is
//! the whole point of that crate: a tool crate that lands is a line there, and
//! nothing in this file changes. What is left here is the binary's half - which
//! document to read, and which session a registry is being built for.

use std::path::Path;
use std::sync::Arc;

use tetanus_config::Config;
use tetanus_engine::agent::ToolScope;
use tetanus_protocol::methods::ToolCatalogResult;
use tetanus_protocol::types as protocol;
use tetanus_session::SessionLog;
use tetanus_toolset::{Composition, Servers};
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::tools::{Tool, ToolRegistry};

use crate::{misconfigured, Policy, Reported};

/// The tools an agent may call. Built from the same assembly a turn is booted
/// with, so `tetanus tools` cannot list a tool a run does not have. It answers
/// `catalog.tools`.
///
/// It reads the document because a listing that ignored it would advertise a
/// set no run offers: the web tools this deployment turned on would be missing,
/// and a source it turned off would be there.
pub fn catalog(
    policy: &Policy,
    document: &Path,
    settings: &Arc<Config>,
    servers: &Servers,
) -> Result<ToolCatalogResult, Reported> {
    Ok(ToolCatalogResult {
        tools: registry(policy, document, &listing(settings, servers))?
            .schemas()
            .into_iter()
            .map(|schema| protocol::ToolDescriptor {
                name: schema.name,
                description: schema.description,
                parameters: schema.parameters,
            })
            .collect(),
    })
}

/// The composition a listing is built against: this deployment's document, and
/// no session.
///
/// A catalogue only reads schemas, so the switch it builds them against is one
/// nothing will ever throw and the session it names is nobody's - no tool is
/// called. Every tool is still *constructed*, because a catalogue that
/// constructed a different set from the one a turn gets is the drift the
/// assembly exists to prevent.
pub fn listing(settings: &Arc<Config>, servers: &Servers) -> Composition {
    Composition::catalogue()
        .settings(Arc::clone(settings))
        .workspace(tetanus_toolset::workspace_root(None))
        .home(Some(tetanus_config::home::home(None)))
        .mcp(servers.tools.clone())
}

/// Start the MCP servers this document declares, saying which did not.
///
/// A server that will not start is a warning and not an error: the run goes
/// on with the tools it does have, which is `crates/mcp`'s rule. What this
/// adds is that it is said at boot, on stderr, rather than discovered later as
/// a tool the model kept not calling.
pub async fn servers(
    policy: &Policy,
    document: &Path,
    settings: &Arc<Config>,
) -> Result<Servers, Reported> {
    let started = Servers::start(settings).await.map_err(|err| {
        misconfigured(
            policy,
            document,
            &tetanus_engine::convert::config_error(&err),
        )
    })?;
    let mut err = policy.stderr();
    for fault in started.faults() {
        err.warn(&fault).ok();
    }
    Ok(started)
}

/// The composition one named session's turns run against.
///
/// Whose tools these are matters twice over. A terminal belongs to the session
/// that opened it, and the registry that holds terminals compares owners
/// exactly, so the owner has to be a real name rather than a constant. And a
/// command whose output outgrows its capture bound has the rest of that output
/// kept on disk, which belongs beside the session's journal - the place a
/// reader is already looking - rather than in a directory this binary invented.
pub fn whose(
    settings: &Arc<Config>,
    mcp: &[Arc<dyn Tool>],
    session_id: &str,
    log: Arc<dyn SessionLog>,
    artifacts: Option<&Path>,
    interrupt: &Arc<Interrupt>,
) -> Composition {
    Composition::for_session(Arc::clone(interrupt), log, session_id)
        .settings(Arc::clone(settings))
        .workspace(tetanus_toolset::workspace_root(None))
        .home(Some(tetanus_config::home::home(None)))
        .artifacts(artifacts)
        // One connection per server per process, shared by every session: an
        // MCP tool belongs to its supervisor, not to whoever called it, and
        // starting a server per session would multiply the child processes by
        // the number of conversations.
        .mcp(mcp.to_vec())
}

/// The one registry, so what is listed and what is callable are one thing.
///
/// A `tools.sources` naming something this build does not ship is the
/// document's mistake, so it is reported as one - against the file it was
/// written in, like every other bad value.
pub fn registry(
    policy: &Policy,
    document: &Path,
    cx: &Composition,
) -> Result<ToolRegistry, Reported> {
    tetanus_toolset::registry(cx).map_err(|err| {
        misconfigured(
            policy,
            document,
            &tetanus_engine::convert::config_error(&err),
        )
    })
}

/// The tools one session runs with, for the engine that serves many sessions
/// at once.
///
/// Each session gets its own, because each has its own stop switch, its own id
/// to key terminals and file observations on, its own place on disk, and its
/// own journal for the feature tools to fold over.
///
/// The document is checked here, before the closure exists, so that a
/// `tools.sources` this build cannot honour is one report at boot rather than a
/// panic on the first prompt: the closure the engine calls has nowhere to
/// return a failure to, and that is a reason to fail early rather than a reason
/// to fail loudly later.
pub fn session_tools(
    policy: &Policy,
    document: &Path,
    settings: &Arc<Config>,
    servers: &Servers,
) -> Result<tetanus_engine::agent::SessionTools, Reported> {
    tetanus_toolset::check(settings).map_err(|err| {
        misconfigured(
            policy,
            document,
            &tetanus_engine::convert::config_error(&err),
        )
    })?;
    let settings = Arc::clone(settings);
    // Cloned once, not per session: the tools hold `Arc`s to the supervisors
    // this process started, so every session shares one connection per server.
    let mcp = servers.tools.clone();
    Ok(Arc::new(
        move |scope: &ToolScope<'_>, interrupt: &Arc<Interrupt>| {
            Arc::new(
                tetanus_toolset::registry(&whose(
                    &settings,
                    &mcp,
                    scope.session_id,
                    Arc::clone(scope.log),
                    scope.artifacts,
                    interrupt,
                ))
                .expect("tools.sources was checked before this closure was built"),
            )
        },
    ))
}

/// The engine configuration every surface that serves the contract must use.
///
/// `catalog.tools` is answered out of [`tetanus_engine::EngineConfig::tools`],
/// and a session's turns dispatch from what `session_tools` builds. A surface
/// that handed the engine a bare `booted` therefore advertises the engine's
/// *offline minimum* - one tool - to every client on a build that offers
/// twenty-six, and dispatches from it too.
///
/// That is not hypothetical, and it is why this exists rather than the four
/// lines it wraps. The composition was written out at the `tetanus serve` call
/// site and nowhere else, so `tetanus serve --frontend` - a second surface,
/// added by another lane, doing the obviously reasonable thing with `booted` -
/// served one tool while `tetanus serve` on the same binary served
/// twenty-six. Two comments in this crate and one in `crates/toolset` said the
/// two could not disagree. They could, because agreeing was a property of one
/// call site rather than of a function both surfaces had to go through.
///
/// A surface that only asks the engine something toolless - `session.list`,
/// `config.dump` - does not need this and does not use it.
pub fn served(
    policy: &Policy,
    document: &Path,
    booted: tetanus_engine::EngineConfig,
    servers: &Servers,
) -> Result<tetanus_engine::EngineConfig, Reported> {
    Ok(tetanus_engine::EngineConfig {
        tools: Arc::new(registry(
            policy,
            document,
            &listing(&booted.resolved, servers),
        )?),
        session_tools: Some(session_tools(policy, document, &booted.resolved, servers)?),
        // Everything else is what the document settled: the provider, model
        // and journal root a served session runs on are its answer and not
        // this file's, which is why the two fields above are set *over*
        // `booted` rather than beside a `..Default::default()`.
        ..booted
    })
}
