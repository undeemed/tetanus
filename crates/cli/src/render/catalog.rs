//! What this build can call: model providers, and tools.
//!
//! `tetanus models` and `tetanus tools` answer the two questions a user has
//! before their first run - what can I point `--model` at, and what can the
//! agent actually do - so both views are written for someone who has not run
//! anything yet.
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
//! # Why parameters are unpacked
//!
//! A tool's parameters are JSON Schema, and printing the schema would answer
//! the question with a document. The three facts a caller needs are the name,
//! the type and whether it is required, so those are lifted out and the rest
//! of the schema is left where it is. A schema this view cannot read is not an
//! error either: the tool is still listed, just without its parameter rows,
//! because a tool you cannot see is worse than a tool you cannot fully read.
//!
//! # Why a narrow window stacks the row
//!
//! A name is what a caller types and what a model asks by, so neither page
//! cuts one. An MCP server's tools are named `server__tool__verb`, and one of
//! those leaves a fifty-column window nothing for a sentence: the description
//! becomes a word and an ellipsis, and the argument rows - which line up
//! under the description - start past the right edge of the page. A provider
//! named at that length pushes its own state off the edge, which is the one
//! thing a reader came to the models page for.
//!
//! So a window that cannot hold the column gets the name on a line of its
//! own, with what follows it indented underneath.

use std::io::{self, Write};

use tetanus_protocol::methods::{ModelCatalogResult, ToolCatalogResult};
use tetanus_protocol::types::{ProviderDescriptor, ToolDescriptor};
use tetanus_ui::{tame, truncate, visible_width, Role, Ui};

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

    // A provider names itself and its models, and a provider is something this
    // build talks to over a network, so every name on this page is tamed.
    let named: Vec<String> = catalog
        .providers
        .iter()
        .map(|provider| tame(&provider.provider))
        .collect();
    let wanted = column(named.iter().map(String::as_str));
    // A provider's state is the reason a reader opened this page - `ready`,
    // or the variable to set - so it is never the thing that runs off the
    // edge. Where the widest name leaves no room for it, it goes under.
    let stacked = ui.width().saturating_sub(wanted + GAP) < LEAST;
    let pad = match stacked {
        true => 0,
        false => wanted,
    };
    for (place, (provider, name)) in catalog.providers.iter().zip(&named).enumerate() {
        if place > 0 {
            ui.blank()?;
        }
        let state = state(ui, provider);
        match stacked {
            true => {
                ui.line(&ui.paint(Role::Accent, name).to_string())?;
                ui.line(&format!("{}{state}", " ".repeat(GAP)))?;
            }
            false => ui.line(&format!(
                "{}{}{state}",
                ui.paint(Role::Accent, name),
                " ".repeat(pad.saturating_sub(visible_width(name)) + GAP)
            ))?,
        }
        for (place, model) in provider.models.iter().enumerate() {
            // Only the first is tagged: tagging every other one `--model` is
            // noise, since that is true of all of them.
            let tag = match place {
                0 => format!("{}{}", " ".repeat(GAP), ui.paint(Role::Muted, "default")),
                _ => String::new(),
            };
            ui.line(&format!("{INDENT}{}{tag}", tame(model)))?;
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
        (false, Some(env)) => ui
            .paint(Role::Warn, &format!("set {}", tame(env)))
            .to_string(),
        (false, None) => ui.paint(Role::Warn, "unavailable").to_string(),
    }
}

/// Render every tool, what it does, and the arguments it takes.
pub fn tools<W: Write>(ui: &mut Ui<W>, catalog: &ToolCatalogResult) -> io::Result<()> {
    ui.heading("tools")?;
    if catalog.tools.is_empty() {
        let empty = ui.paint(Role::Muted, "no tools are registered").to_string();
        return ui.line(&empty);
    }

    let charset = ui.theme().charset();
    let named: Vec<String> = catalog.tools.iter().map(|tool| tame(&tool.name)).collect();
    let wanted = column(named.iter().map(String::as_str));
    // A name is what a caller types and what a model asks by, so it is never
    // cut. When the widest one leaves no sentence beside it, the page stacks
    // instead: the name on its own line, and what it does under it.
    let stacked = ui.width().saturating_sub(wanted + GAP) < LEAST;
    let pad = match stacked {
        true => 0,
        false => wanted,
    };
    let room = ui.width().saturating_sub(pad + GAP).max(1);
    for (place, (tool, name)) in catalog.tools.iter().zip(&named).enumerate() {
        if place > 0 {
            ui.blank()?;
        }
        let said = truncate(&tool.description, room, charset);
        match stacked {
            true => {
                ui.line(&ui.paint(Role::Tool, name).to_string())?;
                ui.line(&format!("{}{said}", " ".repeat(GAP)))?;
            }
            false => ui.line(&format!(
                "{}{}{said}",
                ui.paint(Role::Tool, name),
                " ".repeat(pad.saturating_sub(visible_width(name)) + GAP)
            ))?,
        }
        // The parameters line up under the description, not under the name:
        // they belong to the sentence above them, not to the column of names.
        for argument in arguments(tool) {
            let text = ui.paint(Role::Muted, &argument).to_string();
            ui.line(&format!("{}{text}", " ".repeat(pad + GAP)))?;
        }
    }
    Ok(())
}

/// The narrowest sentence worth a column of its own. Under this, a
/// description beside the name is a word and an ellipsis, and the row it is
/// on has already overrun the window.
const LEAST: usize = 16;

/// The `name (type, required)` rows of one tool, read out of its schema.
///
/// Order is the schema's own, which is stable for a given build, so two runs
/// of `tetanus tools` print the same bytes.
fn arguments(tool: &ToolDescriptor) -> Vec<String> {
    let Some(properties) = tool
        .parameters
        .get("properties")
        .and_then(|p| p.as_object())
    else {
        return Vec::new();
    };
    let required = tool.parameters.get("required").and_then(|r| r.as_array());
    properties
        .iter()
        .map(|(name, spec)| {
            let kind = spec
                .get("type")
                .and_then(|kind| kind.as_str())
                .unwrap_or("any");
            let needed = required
                .map(|list| list.iter().any(|held| held.as_str() == Some(name)))
                .unwrap_or(false);
            let (name, kind) = (tame(name), tame(kind));
            match needed {
                true => format!("{name} ({kind}, required)"),
                false => format!("{name} ({kind})"),
            }
        })
        .collect()
}

/// The widest of a set of cells, in the columns a terminal draws them in.
///
/// Measured in columns and padded in columns, both here: a format width would
/// count the characters of a name instead, and put every row beside a wide one
/// out of place.
fn column<'a>(cells: impl Iterator<Item = &'a str>) -> usize {
    cells.map(visible_width).max().unwrap_or(0)
}

/// Test Design Specification: the catalogue views.
///
/// Features tested: the provider block and its three states; the tag on the
/// model a bare `--adapter` would pick; a provider that advertises nothing; a
/// tool's parameters lifted out of its JSON Schema, including a schema this
/// view cannot read; a description too long for the terminal; both empty
/// catalogues; a name a provider sent that carries escape sequences; a name
/// column beside a name a terminal draws twice as wide; and, on each page, a
/// name so wide that the window has no room left for what belongs beside it.
///
/// Features NOT tested here: which providers and tools this build registers
/// (owned by `main.rs`, and asserted end to end in `tests/presentation.rs`),
/// the JSON forms (owned by `render::json`), and the colour policy (owned by
/// `tetanus-ui`).
///
/// Environmental needs: none. Every case renders into a `Vec<u8>`, so no case
/// reads an environment variable or needs a credential.
#[cfg(test)]
mod tests {
    use serde_json::json;
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

    fn tool(name: &str, description: &str, parameters: serde_json::Value) -> ToolDescriptor {
        ToolDescriptor {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    fn shown(providers: Vec<ProviderDescriptor>, width: usize) -> String {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), width);
        models(&mut ui, &ModelCatalogResult { providers }).expect("render");
        ui.contents()
    }

    fn listed(tools: Vec<ToolDescriptor>, width: usize) -> String {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), width);
        super::tools(&mut ui, &ToolCatalogResult { tools }).expect("render");
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

    /// TC-CLI-CAT-5: a tool whose schema names one required and one optional
    /// argument.
    /// Expected: one row per argument, under the description, each carrying
    /// its type and whether it is required. That is what a caller needs to
    /// write the call; the rest of the schema is not.
    #[test]
    fn every_argument_carries_its_type_and_whether_it_is_required() {
        let out = listed(
            vec![tool(
                "echo",
                "Return the given text unchanged.",
                json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "times": { "type": "integer" }
                    },
                    "required": ["text"],
                }),
            )],
            80,
        );

        assert_eq!(
            out,
            "\ntools\n\
             echo  Return the given text unchanged.\n\
             \x20     text (string, required)\n\
             \x20     times (integer)\n"
        );
    }

    /// TC-CLI-CAT-6: a schema that is not an object with `properties`.
    /// Expected: the tool is still listed, with no argument rows and no panic.
    /// A tool a user cannot see at all is worse than one whose arguments this
    /// build could not read, and the schema is the engine's to shape.
    #[test]
    fn a_schema_this_view_cannot_read_still_lists_its_tool() {
        for schema in [json!({}), json!("free text"), json!({ "type": "string" })] {
            let out = listed(vec![tool("odd", "Takes something else.", schema)], 80);
            assert_eq!(out, "\ntools\nodd  Takes something else.\n");
        }
    }

    /// TC-CLI-CAT-7: a description wider than the terminal.
    /// Expected: the row is cut to the terminal. A description that wrapped
    /// would land under the name column and read as another tool.
    #[test]
    fn a_long_description_is_cut_to_the_terminal() {
        let out = listed(vec![tool("read", &"y".repeat(90), json!({}))], 40);

        for row in out.lines() {
            assert!(row.chars().count() <= 40, "`{row}` overruns 40");
        }
        assert!(out.contains('…'), "the description was not cut:\n{out}");
    }

    /// TC-CLI-CAT-12: a provider whose name leaves no room for its state.
    /// Expected: the name whole on its own line and the state indented under
    /// it, with nothing over the window. The state - `ready`, or the variable
    /// to set - is the reason a reader opened this page, so it is never the
    /// column that runs off the edge.
    #[test]
    fn a_provider_name_that_fills_the_row_puts_its_state_underneath() {
        let name = "deepseek-through-a-corporate-proxy-eu";
        let out = shown(
            vec![provider(name, &["m1"], Some("PROXY_TOKEN"), false)],
            50,
        );

        for row in out.lines() {
            assert!(visible_width(row) <= 50, "`{row}` overruns 50");
        }
        assert!(
            out.lines().any(|row| row.trim() == name),
            "the name is not on a line of its own:\n{out}"
        );
        assert!(
            out.lines()
                .any(|row| row.starts_with("  ") && row.contains("set PROXY_TOKEN")),
            "the state is not under it:\n{out}"
        );
    }

    /// TC-CLI-CAT-11: a tool whose name is wider than the window leaves for a
    /// description.
    /// Expected: nothing overruns, the name is whole - it is what a caller
    /// types into `--tool` and what the model asks for by - and the
    /// description and the arguments are on their own lines under it. An MCP
    /// server's tools are named `server__tool__verb`, so this is the ordinary
    /// case as soon as one is mounted, not a pathological one.
    #[test]
    fn a_name_too_wide_for_the_row_stacks_what_follows_it() {
        let name = "filesystem__read_text_file__with_a_range";
        let out = listed(
            vec![tool(
                name,
                "Read part of a file",
                json!({"properties": {"path": {"type": "string"}}, "required": ["path"]}),
            )],
            50,
        );

        for row in out.lines() {
            assert!(visible_width(row) <= 50, "`{row}` overruns 50");
        }
        assert!(
            out.lines().any(|row| row.trim() == name),
            "the name is not on a line of its own:\n{out}"
        );
        assert!(
            out.contains("Read part of a file"),
            "the description was cut away:\n{out}"
        );
        let argument = out
            .lines()
            .find(|row| row.contains("path"))
            .unwrap_or_else(|| panic!("the argument is on no row:\n{out}"));
        assert!(
            visible_width(argument) <= 50 && argument.starts_with("  "),
            "the argument is off the page: `{argument}`"
        );
    }

    /// TC-CLI-CAT-8: no tools at all.
    /// Expected: the view says so rather than printing a bare heading.
    #[test]
    fn an_empty_tool_list_says_so() {
        assert_eq!(listed(Vec::new(), 80), "\ntools\nno tools are registered\n");
    }

    /// TC-CLI-CAT-9: a provider, a model, a credential variable, a tool and a
    /// parameter whose names carry escape sequences.
    /// Expected: no sequence reaches either page, every name is still read,
    /// and the columns still line up. A provider is something this build
    /// talks to over a network, and its catalogue is its answer, not ours.
    #[test]
    fn a_name_off_the_wire_cannot_drive_the_terminal() {
        let clear = "\u{1b}[2J";
        let page = shown(
            vec![provider(
                &format!("deep{clear}seek"),
                &[&format!("v4{clear}pro")],
                Some(&format!("DEEP{clear}KEY")),
                false,
            )],
            80,
        );
        assert!(!page.contains('\u{1b}'), "{page:?}");
        assert!(page.contains("deepseek"), "{page}");
        assert!(page.contains("v4pro"), "{page}");
        assert!(page.contains("set DEEPKEY"), "{page}");

        let page = listed(
            vec![tool(
                &format!("ec{clear}ho"),
                "Say it back.",
                json!({ "properties": { format!("te{clear}xt"): { "type": format!("str{clear}ing") } } }),
            )],
            80,
        );
        assert!(!page.contains('\u{1b}'), "{page:?}");
        assert!(page.contains("echo"), "{page}");
        assert!(page.contains("text (string)"), "{page}");
    }

    /// TC-CLI-CAT-10: a tool named in a script a terminal draws twice as wide.
    /// Expected: both descriptions start at the same column, counted in what
    /// the terminal draws - fourteen columns of name plus the gap. A column
    /// measured or padded in characters puts the row beside a wide name out of
    /// place by one column for every character of it.
    #[test]
    fn the_name_column_is_measured_and_padded_in_columns() {
        let out = listed(
            vec![
                tool(
                    "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30c4}\u{30fc}\u{30eb}",
                    "Say it wide.",
                    json!({}),
                ),
                tool("read", "Read a file.", json!({})),
            ],
            80,
        );

        for said in ["Say it wide.", "Read a file."] {
            let line = out.lines().find(|line| line.contains(said)).expect(said);
            let at = line.find(said).expect(said);
            assert_eq!(
                visible_width(&line[..at]),
                14 + GAP,
                "the description does not start where the other one does: {line:?}"
            );
        }
    }
}
