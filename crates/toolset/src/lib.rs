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

use std::sync::Arc;

use tetanus_config::{Config, ConfigError};
use tetanus_turn::tools::{EchoTool, Tool, ToolRegistry};

mod assembly;
mod composition;
pub mod servers;

pub use assembly::{Assembly, AssemblyError, Source};
pub use composition::{workspace_root, Composition};
pub use servers::Servers;

/// The settings keys a deployment composes with.
pub mod key {
    /// Which sources to compose. Absent is all of them.
    pub const SOURCES: &str = "tools.sources";
    /// The filesystem mode the file tools are fenced by.
    pub const FS_MODE: &str = "fs.mode";
    /// What every child a composition starts is confined to. Settled strictly
    /// by the engine, and read here so one policy value reaches the shell
    /// tools, the persistent shells and the terminals alike.
    pub const SANDBOX_MODE: &str = "sandbox.mode";
    pub const SANDBOX_WORKSPACE: &str = "sandbox.workspace";
    pub const SANDBOX_NETWORK: &str = "sandbox.network";
    /// The language server the `lsp` tool drives, and the arguments it takes.
    ///
    /// There is no default: "which language server" is a fact about the
    /// project, not about the harness, and a tool that guessed would answer a
    /// model's first question by starting the wrong program.
    pub const LSP_SERVER: &str = "lsp.server";
    pub const LSP_ARGS: &str = "lsp.args";
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
                let sandbox = cx.sandbox();
                tetanus_exec::tools::ShellTools::register_or_explain(
                    registry,
                    Arc::new(tetanus_exec::backend::Bash::new()),
                    tetanus_exec::shell::ShellConfig {
                        spill: spill_to(cx),
                        cwd: sandbox.workspace_root().to_path_buf(),
                        sandbox: sandbox.clone(),
                        ..tetanus_exec::shell::ShellConfig::default()
                    },
                    tetanus_exec::session::SessionConfig {
                        cwd: sandbox.workspace_root().to_path_buf(),
                        sandbox: sandbox.clone(),
                        // The same store the one-shot executor gets: a
                        // persistent shell drops output for the same reason
                        // and the artifacts belong in one place.
                        spill: spill_to(cx),
                        ..tetanus_exec::session::SessionConfig::default()
                    },
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
                    // The attachment store lives in `crates/features`, and
                    // `crates/fs` deliberately does not depend on it: the sink
                    // is a trait there and the composition is where the two
                    // meet, which is what keeps a harness composed without the
                    // feature tools a harness whose file tools still build.
                    .with_images(images(cx))
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
            "lsp",
            "Asking a language server what a symbol is and where it is used.",
            lsp(cx),
        ),
        Source::new(
            "mcp",
            "Tools an MCP server offers, discovered at boot.",
            cx.mcp.clone(),
        ),
    ]
}

/// The language-server tool, if this deployment named a server to run.
///
/// Declared and empty otherwise, for the reason the `web` source is: a source
/// that appears only on the hosts that configured it makes `tools.sources`
/// mean something different on every machine. Empty is also the honest answer
/// here - the client can drive any server that speaks the protocol, and which
/// one belongs to the project rather than to the harness, so a default would
/// be the harness guessing at somebody's toolchain.
fn lsp(cx: &Composition) -> Vec<Arc<dyn Tool>> {
    let Some(program) = cx
        .settings
        .get(key::LSP_SERVER)
        .and_then(|resolved| resolved.value.as_str().map(str::to_owned))
        .filter(|program| !program.trim().is_empty())
    else {
        return Vec::new();
    };
    let args: Vec<String> = cx
        .settings
        .get(key::LSP_ARGS)
        .and_then(|resolved| resolved.value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect();
    let config = tetanus_turn::lsp::LspConfig::new(program, cx.workspace.clone()).with_args(args);
    vec![Arc::new(tetanus_turn::lsp::tool::LspTool::new(config))]
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

    let sandbox = cx.sandbox();
    let terminals = match Terminals::with(
        tetanus_exec::terminal::TerminalConfig {
            cwd: sandbox.workspace_root().to_path_buf(),
            sandbox,
            ..tetanus_exec::terminal::TerminalConfig::default()
        },
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
/// Where a picture `read_image` reads is kept.
///
/// A session with nowhere durable to put one gets the refusing sink rather
/// than a temporary directory, for the reason spill artifacts get none: a file
/// nobody can find later is a file nobody deletes either, and a model told
/// plainly that this build keeps no attachments can do something else.
fn images(cx: &Composition) -> tetanus_fs::image::SharedSink {
    match cx.artifacts.as_ref() {
        Some(artifacts) => Arc::new(AttachmentSink {
            root: tetanus_features::attachment::store_root(artifacts, &cx.session_id),
            log: Arc::clone(&cx.log),
        }),
        None => Arc::new(tetanus_fs::image::NoSink),
    }
}

/// The feature crate's attachment store, behind the file crate's one-method
/// seam.
struct AttachmentSink {
    root: std::path::PathBuf,
    log: Arc<dyn tetanus_session::SessionLog>,
}

impl tetanus_fs::image::ImageSink for AttachmentSink {
    fn admit(&self, name: &str, bytes: Vec<u8>) -> Result<tetanus_fs::image::Stored, String> {
        // The type is read from the bytes rather than from the name, because
        // the name came from a model reading a directory listing and the bytes
        // are the thing being stored. An unrecognised header is stored as a
        // stream of octets rather than refused: the store measures what it can
        // and says nothing it cannot, and a picture this build cannot name is
        // still a picture somebody can open.
        let media_type = media_type_of(&bytes).to_string();
        let incoming = tetanus_features::attachment::Incoming {
            name: name.to_string(),
            media_type,
            bytes,
        };
        // One picture is a batch of one, so the whole-batch admission rule
        // applies unchanged: nothing is stored unless everything is
        // admissible, which for a batch of one is the plain reading anyway.
        let mut admitted = tetanus_features::attachment::attach(
            self.log.as_ref(),
            &self.root,
            std::slice::from_ref(&incoming),
            &tetanus_features::attachment::Limits::default(),
        )
        .map_err(|error| error.to_string())?;
        let stored = admitted.pop().ok_or("the store admitted nothing")?;
        Ok(tetanus_fs::image::Stored {
            id: stored.id,
            media_type: stored.media_type,
            bytes: stored.bytes,
            dimensions: stored.dimensions.map(|size| (size.width, size.height)),
        })
    }
}

/// What a picture is, according to its first bytes.
///
/// The four signatures `crates/features` can already measure, and nothing
/// else: a longer table would be a second sniffing implementation to keep in
/// step with the one that reads dimensions.
fn media_type_of(bytes: &[u8]) -> &'static str {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    const GIF: &[u8] = b"GIF8";
    match bytes {
        _ if bytes.starts_with(PNG) => "image/png",
        _ if bytes.starts_with(&[0xff, 0xd8, 0xff]) => "image/jpeg",
        _ if bytes.starts_with(GIF) => "image/gif",
        _ if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" => {
            "image/webp"
        }
        _ => "application/octet-stream",
    }
}

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
