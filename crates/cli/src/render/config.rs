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
//!
//! # Why the document is on the heading
//!
//! Half the question is "where do I change it", and a `file` layer on a row
//! answers only half of that: which file is a fact about the machine this ran
//! on, not about the row. So the document the page read is drawn beside the
//! heading, where it is read once rather than repeated down a column.
//!
//! A key, its value and the layer that settled it all came out of a file, an
//! environment or a flag, so all three are tamed before they are drawn. The
//! value is tamed by the width rule that cuts it; the other two are drawn as
//! themselves and are tamed here.

use std::io::{self, Write};

use tetanus_protocol::types::{ConfigEntry, ConfigLayer};
use tetanus_ui::{tame, truncate, visible_width, Role, Ui};

/// Space between the key, value and layer columns.
const GAP: usize = 2;

/// Render every resolved key on one aligned table, headed by the document
/// this page read.
///
/// `read` is `None` for the page that read no document at all, where naming
/// one would be naming a file this answer did not come from.
pub fn render<W: Write>(
    ui: &mut Ui<W>,
    entries: &[ConfigEntry],
    read: Option<&str>,
) -> io::Result<()> {
    match read {
        Some(document) => ui.heading_at("config", document)?,
        None => ui.heading("config")?,
    }
    if entries.is_empty() {
        // An empty table is a fact, not an error: a build with no defaults is
        // a build that resolved nothing, and saying so beats printing a
        // heading with nothing under it.
        let empty = ui.paint(Role::Muted, "nothing is set").to_string();
        return ui.line(&empty);
    }

    let charset = ui.theme().charset();
    let named: Vec<String> = entries.iter().map(|entry| tame(&entry.key)).collect();
    let keys = column(named.iter().map(String::as_str));
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

    for ((key, value), layer) in named.iter().zip(&values).zip(&layers) {
        // Both the value and the layer are one column each, so the gap is
        // measured here and the layer is painted afterwards: a painted string
        // carries escapes a format width would count.
        let pad = " ".repeat(width.saturating_sub(visible_width(value)) + GAP);
        let layer = ui.paint(Role::Muted, layer).to_string();
        ui.field(key, keys, &format!("{value}{pad}{layer}"))?;
    }
    Ok(())
}

/// The widest of a set of cells, in the columns a terminal draws them in.
fn column<'a>(cells: impl Iterator<Item = &'a str>) -> usize {
    cells.map(visible_width).max().unwrap_or(0)
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
        ConfigLayer::Other(name) => tame(name),
    }
}

/// Test Design Specification: the config table.
///
/// Features tested: column alignment, how a JSON value is spelled, a value too
/// long for the terminal, a layer this build does not know, an empty table, a
/// key and a layer that carry escape sequences, a key column beside a key a
/// terminal draws twice as wide, and the document on the heading against the
/// page that read none. Features NOT tested here: which layer wins a key
/// (owned by `tetanus-config`), which document this run reads and how its
/// path is written (owned by the binary, and asserted end to end in
/// `tests/presentation.rs`), and the colour policy (owned by `tetanus-ui`).
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
        headed(entries, width, None)
    }

    fn headed(entries: &[ConfigEntry], width: usize, read: Option<&str>) -> String {
        let mut ui = buffered(Theme::new(false, Charset::Unicode), width);
        render(&mut ui, entries, read).expect("render");
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

    /// TC-CLI-CFG-6: a key and a layer that carry escape sequences.
    /// Expected: no sequence reaches the page and both are still read. A key
    /// comes out of a file, an environment or a flag, and a layer this build
    /// does not know is a word the engine chose - none of the three is ours.
    #[test]
    fn a_key_out_of_a_file_is_drawn_and_not_obeyed() {
        let clear = "\u{1b}[2J";
        let out = rendered(
            &[entry(
                &format!("log{clear}.level"),
                json!("trace"),
                ConfigLayer::Other(format!("work{clear}space")),
            )],
            80,
        );

        assert!(!out.contains('\u{1b}'), "{out:?}");
        assert_eq!(out, "\nconfig\nlog.level  trace  workspace\n");
    }

    /// TC-CLI-CFG-7: a key in a script a terminal draws twice as wide.
    /// Expected: both values start at the same column, counted in what the
    /// terminal draws. A key column padded in characters would put the value
    /// beside a wide key out of place, and the layer column with it.
    #[test]
    fn the_key_column_is_measured_and_padded_in_columns() {
        let out = rendered(
            &[
                entry("\u{65e5}\u{672c}\u{8a9e}", json!("wide"), ConfigLayer::File),
                entry("log.level", json!("trace"), ConfigLayer::File),
            ],
            80,
        );

        for value in ["wide", "trace"] {
            let line = out.lines().find(|line| line.contains(value)).expect(value);
            let at = line.find(value).expect(value);
            assert_eq!(
                visible_width(&line[..at]),
                "log.level".len() + GAP,
                "the value does not start where the other one does: {line:?}"
            );
        }
    }

    /// TC-CLI-CFG-8: the document the page read, and the page that read none.
    /// Expected: the document sits on the heading row, two spaces after the
    /// title, and every row under it is unchanged - the reader who asks "where
    /// do I change it" is answered once, not down a column. A page told of no
    /// document is headed by the bare title, because naming a file that
    /// answer did not come from would be worse than naming none.
    #[test]
    fn the_page_says_which_document_it_read() {
        let entries = [entry("model", json!("deepseek-v4"), ConfigLayer::File)];

        let named = headed(&entries, 80, Some("/home/u/.tetanus/settings.yaml"));
        assert_eq!(
            named,
            "\nconfig  /home/u/.tetanus/settings.yaml\nmodel  deepseek-v4  file\n"
        );

        let none = headed(&entries, 80, None);
        assert_eq!(none, "\nconfig\nmodel  deepseek-v4  file\n");
    }
}
