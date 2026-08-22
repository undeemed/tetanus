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
use tetanus_toolset::Composition;
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::tools::ToolRegistry;

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
) -> Result<ToolCatalogResult, Reported> {
    Ok(ToolCatalogResult {
        tools: registry(policy, document, &listing(settings))?
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
pub fn listing(settings: &Arc<Config>) -> Composition {
    Composition::catalogue()
        .settings(Arc::clone(settings))
        .workspace(tetanus_toolset::workspace_root(None))
        .home(Some(tetanus_config::home::home(None)))
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
) -> Result<tetanus_engine::agent::SessionTools, Reported> {
    tetanus_toolset::check(settings).map_err(|err| {
        misconfigured(
            policy,
            document,
            &tetanus_engine::convert::config_error(&err),
        )
    })?;
    let settings = Arc::clone(settings);
    Ok(Arc::new(
        move |scope: &ToolScope<'_>, interrupt: &Arc<Interrupt>| {
            Arc::new(
                tetanus_toolset::registry(&whose(
                    &settings,
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
