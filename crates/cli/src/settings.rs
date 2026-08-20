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
use std::path::Path;

use tetanus_ui::{Policy, Ui};

use crate::render;
use crate::{fail, misconfigured, report, Reported};

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
    let document = tetanus_config::file::document_path(&tetanus_config::home::home(None));
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
