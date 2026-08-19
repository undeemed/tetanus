//! What this build can call: the model providers, and what they advertise.
//!
//! `tetanus models` answers the question a user has before their first run -
//! what can I point `--model` at, and will it work - so the view is written
//! for someone who has not run anything yet.
//!
//! # Why availability is on the provider row
//!
//! A provider whose credential is absent is the single most common reason a
//! first run fails, and the failure arrives late, from inside a turn. Naming
//! the variable here turns that into something a user fixes before they start.
//! It is a state, not an error: a build that lists a provider it cannot reach
//! is correctly configured and exits `0`.
//!
//! # Why the first model is tagged
//!
//! `tetanus run --adapter <name>` with no `--model` takes the first model the
//! provider advertises. That rule lives in the binary, not in the contract, so
//! the tag belongs to this view: the catalog order is contract data, and what
//! the CLI does with the order is what the tag reports.
//!
//! An unlisted model id still passes through to the provider - the catalog is
//! advisory (contract §4.3) - so this view never reads as a whitelist.
//!
use std::io::{self, Write};

use tetanus_protocol::methods::ModelCatalogResult;
use tetanus_protocol::types::ProviderDescriptor;
use tetanus_ui::{Role, Ui};

/// Indent of a row that belongs to the entry above it.
const INDENT: &str = "  ";

/// Space between the two columns of a row.
const GAP: usize = 2;

/// Render every provider, its state, and the models it advertises.
pub fn models<W: Write>(ui: &mut Ui<W>, catalog: &ModelCatalogResult) -> io::Result<()> {
    ui.heading("models")?;
    if catalog.providers.is_empty() {
        let empty = ui
            .paint(Role::Muted, "no providers are registered")
            .to_string();
        return ui.line(&empty);
    }

    let pad = column(catalog.providers.iter().map(|p| p.provider.as_str()));
    for (place, provider) in catalog.providers.iter().enumerate() {
        if place > 0 {
            ui.blank()?;
        }
        let state = state(ui, provider);
        ui.line(&format!(
            "{:<pad$}{}{state}",
            ui.paint(Role::Accent, &provider.provider),
            " ".repeat(GAP)
        ))?;
        for (place, model) in provider.models.iter().enumerate() {
            // Only the first is tagged: tagging every other one `--model` is
            // noise, since that is true of all of them.
            let tag = match place {
                0 => format!("{}{}", " ".repeat(GAP), ui.paint(Role::Muted, "default")),
                _ => String::new(),
            };
            ui.line(&format!("{INDENT}{model}{tag}"))?;
        }
        if provider.models.is_empty() {
            let none = ui
                .paint(Role::Muted, "no advertised models - name one with --model")
                .to_string();
            ui.line(&format!("{INDENT}{none}"))?;
        }
    }
    Ok(())
}

/// A provider's state, in the words that say what to do about it.
fn state<W: Write>(ui: &Ui<W>, provider: &ProviderDescriptor) -> String {
    match (provider.available, provider.credential_env.as_deref()) {
        (true, _) => ui.paint(Role::Ok, "ready").to_string(),
        (false, Some(env)) => ui.paint(Role::Warn, &format!("set {env}")).to_string(),
        (false, None) => ui.paint(Role::Warn, "unavailable").to_string(),
    }
}

/// The widest of a set of cells, in characters a terminal draws.
fn column<'a>(cells: impl Iterator<Item = &'a str>) -> usize {
    cells.map(|cell| cell.chars().count()).max().unwrap_or(0)
}

/// Test Design Specification: the provider view.
///
/// Features tested: the provider block and its three states; the tag on the
/// model a bare `--adapter` would pick; a provider that advertises nothing;
/// and an empty catalogue.
///
/// Features NOT tested here: which providers this build registers (owned by
/// `main.rs`, and asserted end to end in `tests/presentation.rs`), the JSON
/// form (owned by `render::json`), and the colour policy (owned by
/// `tetanus-ui`).
///
/// Environmental needs: none. Every case renders into a `Vec<u8>`, so no case
/// reads an environment variable or needs a credential.
#[cfg(test)]
mod tests {
    use tetanus_ui::{buffered, Charset, Theme};

    use super::*;

    fn provider(
        name: &str,
        models: &[&str],
        env: Option<&str>,
        available: bool,
    ) -> ProviderDescriptor {
        ProviderDescriptor {
            provider: name.into(),
            models: models.iter().map(|model| (*model).into()).collect(),
            credential_env: env.map(Into::into),
            available,
        }
    }

    fn shown(providers: Vec<ProviderDescriptor>, width: usize) -> String {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), width);
        models(&mut ui, &ModelCatalogResult { providers }).expect("render");
        ui.contents()
    }

    /// TC-CLI-CAT-1: two providers, one reachable and one not.
    /// Expected: the provider names share a column, each block lists its own
    /// models, and the unreachable one names the variable to set. A user who
    /// reads this page knows what to fix before a turn fails from inside.
    #[test]
    fn a_provider_that_cannot_be_reached_names_the_variable_to_set() {
        let out = shown(
            vec![
                provider("mock", &["mock-echo"], None, true),
                provider(
                    "deepseek-official",
                    &["deepseek-chat", "deepseek-reasoner"],
                    Some("DEEPSEEK_API_KEY"),
                    false,
                ),
            ],
            80,
        );

        assert_eq!(
            out,
            "\nmodels\n\
             mock               ready\n\
             \x20 mock-echo  default\n\
             \n\
             deepseek-official  set DEEPSEEK_API_KEY\n\
             \x20 deepseek-chat  default\n\
             \x20 deepseek-reasoner\n"
        );
    }

    /// TC-CLI-CAT-2: a provider registered without a credential variable and
    /// still unavailable.
    /// Expected: `unavailable`, not an empty column. The contract allows a
    /// provider with no `credential_env`, and a blank state would read as
    /// "ready" to anyone scanning the column.
    #[test]
    fn an_unavailable_provider_with_no_variable_still_says_so() {
        let out = shown(vec![provider("local", &["gguf"], None, false)], 80);
        assert!(out.contains("local  unavailable"), "{out}");
    }

    /// TC-CLI-CAT-3: a provider that advertises nothing.
    /// Expected: the block says the catalog is empty and points at `--model`.
    /// The catalog is advisory, so an unlisted id still runs; a bare provider
    /// name with nothing under it would read as "this one is broken".
    #[test]
    fn a_provider_with_no_catalog_points_at_the_flag() {
        let out = shown(vec![provider("openai-compatible", &[], None, true)], 80);
        assert!(out.contains("name one with --model"), "{out}");
    }

    /// TC-CLI-CAT-4: no providers at all.
    /// Expected: the view says so rather than printing a bare heading.
    #[test]
    fn an_empty_provider_list_says_so() {
        assert_eq!(
            shown(Vec::new(), 80),
            "\nmodels\nno providers are registered\n"
        );
    }
}
