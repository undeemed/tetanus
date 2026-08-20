//! The settings document every engine this binary builds is booted from, and
//! the page that reports it.
//!
//! `tetanus config` answers one question - what is this harness configured to
//! do, and which layer decided it - and the only honest answer is the one the
//! engine itself resolved. So [`booted`] is the one place a subcommand turns a
//! document and a few flags into an [`EngineConfig`](tetanus_engine::EngineConfig),
//! and [`page`] reports what that boot produced. A surface that read
//! `settings.yaml` for itself would be a second answer to that one question,
//! and the two would drift the first time a key changed hands.
//!
//! It lives beside [`crate::chat`] rather than in `render` for the same reason
//! `chat` does: `render` draws a value it is handed, and this resolves one.
//! The drawing is still `render::config`'s. And it lives out of `main.rs`
//! because `main.rs` is the binary's hub - argv, dispatch, the wiring every
//! command reaches through - and a command that keeps its body there makes the
//! hub wider for every other command.

use std::io::Write;
use std::path::{Path, PathBuf};

use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_ui::{Policy, Ui};

use crate::render;
use crate::{fail, misconfigured, report, AdapterChoice, Reported};

/// Write the page for the settings the next command would run on.
///
/// A flag is only on the `Flag` layer of the process it was typed at, so a
/// page with no flags can never print `flag` - and the flag layer is exactly
/// what a reader comes to this command to understand. `--dir` is that flag,
/// asked here as a question rather than as an instruction: it lists nothing
/// and opens nothing, it says what a subcommand given it would run on.
///
/// The page is the engine's own `config.dump`, not a copy of the resolved
/// layers: the engine reports the value it will actually use for a key it
/// settles, and it drops the value of a key whose name says it holds a
/// credential (contract §4.3). A surface that printed the layers itself would
/// print that credential.
pub fn page<W: Write>(
    policy: &Policy,
    out: &mut Ui<W>,
    dir: Option<&Path>,
    json: bool,
) -> Result<(), Reported> {
    let engine = tetanus_engine::HarnessEngine::new(booted(policy, &root(dir))?);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| report(policy, &err.to_string(), None))?;
    let dump = runtime
        .block_on(tetanus_protocol::methods::Engine::config_dump(&engine))
        .map_err(|err| fail(policy, &err))?;
    if json {
        return render::json::line(out, &dump)
            .map_err(|err| report(policy, &err.to_string(), None));
    }
    render::config::render(out, &dump.entries).ok();
    Ok(())
}

/// The settings the harness will run on: the document under the harness home,
/// with whatever the user typed on top of it.
///
/// Every subcommand that builds an engine comes through here, so `tetanus
/// config` describes the settings the next command will actually use. A
/// subcommand that resolved its own would be a second answer to the question
/// this binary has one command for.
///
/// `flags` are the keys the user set on the command line, on the layer that
/// says so: `Flag` outranks `File`, which is what makes a flag win, and
/// `config.dump` then reports the value as `flag`. A flag the user did not
/// pass is not in the list at all - a clap default put on this layer would be
/// a document that could never win.
///
/// Neither step falls back on failure. A document the harness ignored leaves
/// the user configured on paper and unconfigured in fact.
pub fn booted(
    policy: &Policy,
    flags: &[(&'static str, serde_json::Value)],
) -> Result<tetanus_engine::EngineConfig, Reported> {
    let document = document();
    let mut settings = tetanus_engine::boot::document(&document).map_err(|err| {
        misconfigured(
            policy,
            &document,
            &tetanus_engine::convert::config_error(&err),
        )
    })?;
    for (key, value) in flags {
        settings.set(key, value.clone(), tetanus_config::Layer::Flag);
    }
    tetanus_engine::EngineConfig::from_settings(settings).map_err(|err| {
        let fault = tetanus_engine::convert::config_error(&err);
        // Whoever set the value is who the report is for. A value the flags
        // put there is a flag to retype, and `fail` sends the reader to
        // `--help` as it does for any other bad argument; everything else
        // came off the document, and is fixed by editing it.
        let blamed = fault
            .data
            .as_ref()
            .and_then(|data| data.get("field"))
            .and_then(serde_json::Value::as_str);
        match blamed.is_some_and(|key| flags.iter().any(|(set, _)| *set == key)) {
            true => fail(policy, &fault),
            false => misconfigured(policy, &document, &fault),
        }
    })
}

/// `--dir` as the one settings key it overrides, or nothing when it was not
/// passed. Nothing is the point: it is what leaves the document able to win.
pub fn root(dir: Option<&Path>) -> Vec<(&'static str, serde_json::Value)> {
    dir.map(|dir| {
        (
            tetanus_engine::catalog::key::SESSIONS_ROOT,
            serde_json::json!(dir.display().to_string()),
        )
    })
    .into_iter()
    .collect()
}

/// Where this run's settings document lives.
pub fn document() -> PathBuf {
    tetanus_config::file::document_path(&tetanus_config::home::home(None))
}

/// The flags a command that runs turns was given, before the document has
/// been read. Every one of them is optional for the reason [`root`] gives.
pub struct TurnFlags {
    pub adapter: Option<AdapterChoice>,
    pub model: Option<String>,
    pub max_steps: Option<u32>,
    pub session: Option<PathBuf>,
}

/// What a turn will run on, once the document and the flags have both been
/// read.
pub struct Turn {
    /// The settled settings, so the engine the turn opens on is the one
    /// `tetanus config` described.
    pub settings: tetanus_engine::EngineConfig,
    pub provider: AdapterChoice,
    /// `None` leaves the model to the adapter's own catalogue, which is the
    /// only sensible default for an adapter nobody configured.
    pub model: Option<String>,
    pub max_steps: u32,
    pub journal: PathBuf,
}

/// Resolve one turn's settings out of the document and the flags.
///
/// `fallback` is the provider the command has when nobody said otherwise, and
/// it is not always the engine's: `tetanus run` is the mock adapter because a
/// first run must need no credential, and `tetanus chat` is DeepSeek because
/// a conversation with the mock is a demonstration rather than a use. Which
/// is why the compiled default of a key cannot decide this - only a layer
/// somebody wrote can, and that is what the layer is read for below.
pub fn turn_settings(
    policy: &Policy,
    flags: TurnFlags,
    fallback: AdapterChoice,
    journal: &str,
) -> Result<Turn, Reported> {
    use tetanus_engine::catalog::key;

    let mut overrides: Vec<(&'static str, serde_json::Value)> = Vec::new();
    if let Some(adapter) = flags.adapter {
        overrides.push((key::PROVIDER, serde_json::json!(adapter.route())));
    }
    if let Some(model) = &flags.model {
        overrides.push((key::MODEL, serde_json::json!(model)));
    }
    if let Some(steps) = flags.max_steps {
        overrides.push((key::MAX_STEPS, serde_json::json!(steps)));
    }
    let settings = booted(policy, &overrides)?;

    // A key still on its compiled layer is a key nobody has an opinion about,
    // and the command's own default stands. Anything above it - a document, an
    // environment, a flag - is somebody's opinion, and outranks the default
    // this binary happens to compile in.
    let written = |key: &str| {
        settings
            .resolved
            .get(key)
            .is_some_and(|resolved| resolved.layer > tetanus_config::Layer::Default)
    };

    Ok(Turn {
        provider: match written(key::PROVIDER) {
            true => provider_named(policy, &settings.default_provider)?,
            false => fallback,
        },
        // Not the settled value when nobody set it: the engine's compiled
        // model belongs to the engine's compiled provider, and offering it to
        // an adapter that never advertised it would name a model that does
        // not exist. An unset model is the adapter's first catalogue entry.
        model: written(key::MODEL).then(|| settings.default_model.clone()),
        max_steps: settings.max_steps,
        journal: flags
            .session
            .unwrap_or_else(|| settings.sessions_root.join(journal)),
        settings,
    })
}

/// The adapter a provider name asks for.
///
/// clap refuses an unknown `--adapter`, so a name that gets this far came out
/// of a document or an environment, and is reported the way any other value
/// in one is: it names the key, and it names the file that has to be edited.
pub fn provider_named(policy: &Policy, name: &str) -> Result<AdapterChoice, Reported> {
    [AdapterChoice::Mock, AdapterChoice::Deepseek]
        .into_iter()
        .find(|choice| choice.route() == name)
        .ok_or_else(|| {
            let known = [AdapterChoice::Mock, AdapterChoice::Deepseek]
                .map(AdapterChoice::route)
                .join(" or ");
            misconfigured(
                policy,
                &document(),
                &RpcError::new(
                    ErrorCode::InvalidParams,
                    format!("must be a provider this build can reach, {known}, not {name:?}"),
                )
                .with_data(serde_json::json!({
                    "field": tetanus_engine::catalog::key::PROVIDER,
                })),
            )
        })
}
