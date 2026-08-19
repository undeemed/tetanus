//! Resolved configuration, and who resolved it.
//!
//! The value on its own is not what a user opens this view for. The question
//! behind `tetanus config` is nearly always "why is it that, and where do I
//! change it", so the layer that settled a key sits on the same row as the
//! value it settled. The table is ordered by key, not by layer: a reader looks
//! a key up first and only then wonders where it came from.
//!
//! Values are printed the way a person writes them into a config file, so a
//! string loses its JSON quotes. Every other shape keeps its own spelling,
//! which is what still tells `true` the boolean apart from `"true"` the string.

use std::io::{self, Write};

use tetanus_protocol::types::{ConfigEntry, ConfigLayer};
use tetanus_ui::{truncate, Role, Ui};

/// Space between the key, value and layer columns.
const GAP: usize = 2;

/// Render every resolved key on one aligned table.
pub fn render<W: Write>(ui: &mut Ui<W>, entries: &[ConfigEntry]) -> io::Result<()> {
    ui.heading("config")?;
    if entries.is_empty() {
        // An empty table is a fact, not an error: a build with no defaults is
        // a build that resolved nothing, and saying so beats printing a
        // heading with nothing under it.
        let empty = ui.paint(Role::Muted, "nothing is set").to_string();
        return ui.line(&empty);
    }

    let charset = ui.theme().charset();
    let keys = column(entries.iter().map(|entry| entry.key.as_str()));
    let layers: Vec<String> = entries.iter().map(|entry| layer(&entry.layer)).collect();

    // Whatever the two outer columns leave over is the value's. A long value
    // is cut rather than folded: a folded value would put text under the
    // layer column, and the layer is the part of this view that has to stay
    // scannable.
    let room = ui
        .width()
        .saturating_sub(keys + column(layers.iter().map(String::as_str)) + GAP * 2);
    let values: Vec<String> = entries
        .iter()
        .map(|entry| truncate(&value(&entry.value), room, charset))
        .collect();
    let width = column(values.iter().map(String::as_str));

    for ((entry, value), layer) in entries.iter().zip(&values).zip(&layers) {
        // Both the value and the layer are one column each, so the gap is
        // measured in characters and the layer is painted afterwards: a
        // painted string carries escapes a format width would count.
        let pad = " ".repeat(width.saturating_sub(value.chars().count()) + GAP);
        let layer = ui.paint(Role::Muted, layer).to_string();
        ui.field(&entry.key, keys, &format!("{value}{pad}{layer}"))?;
    }
    Ok(())
}

/// The widest of a set of cells, in characters a terminal draws.
fn column<'a>(cells: impl Iterator<Item = &'a str>) -> usize {
    cells.map(|cell| cell.chars().count()).max().unwrap_or(0)
}

/// A JSON value as a person would have typed it.
fn value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The layer, in the words the config docs use. An `Other` layer is shown
/// exactly as the engine spelled it, which is what lets the engine add a layer
/// in a minor version without this view failing (contract §2).
fn layer(layer: &ConfigLayer) -> String {
    match layer {
        ConfigLayer::Default => "default".into(),
        ConfigLayer::File => "file".into(),
        ConfigLayer::Env => "env".into(),
        ConfigLayer::Flag => "flag".into(),
        ConfigLayer::Other(name) => name.clone(),
    }
}

/// Test Design Specification: the config table.
///
/// Features tested: column alignment, how a JSON value is spelled, a value too
/// long for the terminal, a layer this build does not know, and an empty
/// table. Features NOT tested here: which layer wins a key (owned by
/// `tetanus-config`) and the colour policy (owned by `tetanus-ui`).
///
/// Environmental needs: none. Every case renders into a `Vec<u8>`.
#[cfg(test)]
mod tests {
    use serde_json::json;
    use tetanus_ui::{buffered, Charset, Theme};

    use super::*;

    fn entry(key: &str, value: serde_json::Value, layer: ConfigLayer) -> ConfigEntry {
        ConfigEntry {
            key: key.into(),
            value,
            layer,
        }
    }

    fn rendered(entries: &[ConfigEntry], width: usize) -> String {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), width);
        render(&mut ui, entries).expect("render");
        ui.contents()
    }

    /// TC-CLI-CFG-1: a table of keys of different lengths.
    /// Expected: three columns, each starting at one place on every row, so a
    /// reader scans the layers down a straight edge.
    #[test]
    fn every_column_starts_in_one_place() {
        let out = rendered(
            &[
                entry("log.level", json!("info"), ConfigLayer::Default),
                entry("model", json!("deepseek-v4"), ConfigLayer::Flag),
                entry("session.dir", json!("sessions"), ConfigLayer::File),
            ],
            80,
        );

        assert_eq!(
            out,
            "\nconfig\nlog.level    info         default\n\
             model        deepseek-v4  flag\n\
             session.dir  sessions     file\n"
        );
    }

    /// TC-CLI-CFG-2: the JSON shapes a config file can hold.
    /// Expected: a string is printed bare, and everything else keeps its JSON
    /// spelling. Without that, `true` and `"true"` would read the same, and a
    /// user cannot fix a type error they cannot see.
    #[test]
    fn a_string_loses_its_quotes_and_nothing_else_does() {
        let out = rendered(
            &[
                entry("a.text", json!("plain"), ConfigLayer::File),
                entry("a.bool", json!(true), ConfigLayer::File),
                entry("a.number", json!(8), ConfigLayer::File),
                entry("a.list", json!(["one", "two"]), ConfigLayer::File),
            ],
            80,
        );

        let values: Vec<&str> = out
            .lines()
            .skip(2)
            .filter_map(|line| line.split_whitespace().nth(1))
            .collect();
        assert_eq!(values, ["plain", "true", "8", "[\"one\",\"two\"]"]);
    }

    /// TC-CLI-CFG-3: a value wider than the terminal.
    /// Expected: the row still fits the terminal and the layer is still on it.
    /// A value that wrapped would push the layer onto a line of its own, where
    /// it no longer reads as that key's provenance.
    #[test]
    fn a_long_value_is_cut_so_the_layer_stays_on_the_row() {
        let out = rendered(
            &[
                entry("short", json!("x"), ConfigLayer::Env),
                entry("long", json!("y".repeat(90)), ConfigLayer::Env),
            ],
            40,
        );

        let rows: Vec<&str> = out.lines().skip(2).collect();
        for row in &rows {
            assert!(row.chars().count() <= 40, "`{row}` overruns 40");
            assert!(row.ends_with("env"), "`{row}` lost its layer");
        }
        assert!(rows[1].contains('…'), "`{}` was not cut", rows[1]);
    }

    /// TC-CLI-CFG-4: a layer added after this build.
    /// Expected: it is shown as the engine spelled it. The contract's `Other`
    /// fallback is only worth having if a surface renders it (contract §2).
    #[test]
    fn a_layer_this_build_never_heard_of_is_still_shown() {
        let out = rendered(
            &[entry(
                "log.level",
                json!("trace"),
                ConfigLayer::Other("workspace".into()),
            )],
            80,
        );

        assert_eq!(out, "\nconfig\nlog.level  trace  workspace\n");
    }

    /// TC-CLI-CFG-5: nothing resolved.
    /// Expected: the view says so rather than printing a bare heading.
    #[test]
    fn an_empty_table_says_so() {
        assert_eq!(rendered(&[], 80), "\nconfig\nnothing is set\n");
    }
}
