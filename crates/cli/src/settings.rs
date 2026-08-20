//! The settings document the engine boots from, read for the page that
//! reports it.
//!
//! `tetanus config` answers one question - what is this harness configured to
//! do, and which layer decided it - and the only honest answer is the one the
//! engine itself resolved. So this module boots the document the way the
//! engine boots it, and turns what came back into rows. A surface that read
//! `settings.yaml` for itself would be a second answer to that one question,
//! and the two would drift the first time a key changed hands.
//!
//! It lives beside [`crate::chat`] rather than in `render` for the same reason
//! `chat` does: `render` draws a value it is handed, and this reads one. The
//! drawing is still `render::config`'s.

use std::io::Write;

use tetanus_protocol::methods::ConfigDumpResult;
use tetanus_protocol::types as protocol;
use tetanus_ui::{Policy, Ui};

use crate::render;
use crate::{misconfigured, report, Reported};

/// Write the page for the document this process would boot from.
///
/// Both steps can fail, and neither failure is stepped over: a document the
/// harness ignored leaves the user configured on paper and unconfigured in
/// fact, which is exactly the state this command exists to expose.
pub fn page<W: Write>(policy: &Policy, out: &mut Ui<W>, json: bool) -> Result<(), Reported> {
    let dump = ConfigDumpResult {
        entries: resolved(policy)?,
    };
    if json {
        return render::json::line(out, &dump)
            .map_err(|err| report(policy, &err.to_string(), None));
    }
    render::config::render(out, &dump.entries).ok();
    Ok(())
}

/// The keys the engine has, each with the layer that settled it.
///
/// The list is the engine's, so the keys a document may set and the keys the
/// page shows cannot drift apart, and a key the engine gains appears here
/// without this crate being told about it.
fn resolved(policy: &Policy) -> Result<Vec<protocol::ConfigEntry>, Reported> {
    let document = tetanus_config::file::document_path(&tetanus_config::home::home(None));
    let engine = tetanus_engine::boot::document(&document)
        .and_then(tetanus_engine::EngineConfig::from_settings)
        .map_err(|err| {
            misconfigured(
                policy,
                &document,
                &tetanus_engine::convert::config_error(&err),
            )
        })?;
    Ok(entries(&engine.resolved))
}

/// One row per key: what it is set to, and which layer set it.
fn entries(config: &tetanus_config::Config) -> Vec<protocol::ConfigEntry> {
    config
        .provenance()
        .map(|(key, resolved)| protocol::ConfigEntry {
            key: key.clone(),
            value: resolved.value.clone(),
            layer: match resolved.layer {
                tetanus_config::Layer::Default => protocol::ConfigLayer::Default,
                tetanus_config::Layer::File => protocol::ConfigLayer::File,
                tetanus_config::Layer::Env => protocol::ConfigLayer::Env,
                tetanus_config::Layer::Flag => protocol::ConfigLayer::Flag,
            },
        })
        .collect()
}
