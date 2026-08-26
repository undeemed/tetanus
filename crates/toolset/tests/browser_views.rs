//! Every tool this build offers is accounted for by the browser panel.
//!
//! This crate's own note promises that "a tool crate that lands adds exactly
//! one entry here and changes nothing else in the workspace". That is true of
//! the engine and false of the surfaces: a tool added to [`sources`] is a tool
//! the model can call and `tetanus tools` lists, and the browser panel goes on
//! drawing it as a JSON tree until somebody notices.
//!
//! Somebody noticing is how it has worked so far. Diffing `tetanus tools`
//! against the page's view table by hand found seven feature tools with no
//! view, weeks after they landed. This is that diff, run by the suite.
//!
//! # Why it lives here
//!
//! Because [`stock_registry`] is here, and the alternative was a test in
//! `crates/host` with a dev-dependency on this crate - a crate edge from the
//! thing that serves files to the thing that knows about tools, added for one
//! assertion. Reading two JavaScript files as text adds no edge at all.
//!
//! # What "accounted for" means
//!
//! Either the page has a view, or the tool is named in [`STILL_BARE`] with the
//! reason. The list is asserted in **both** directions: a new tool with no view
//! fails until somebody decides about it, and a tool that gains a view fails
//! until it leaves the list. Neither half can rot quietly.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Tools this build offers that the browser panel draws with the generic
/// frame - name, arguments as a tree, result, and whether it worked.
///
/// The generic frame is honest and complete, so being here is a decision
/// rather than a defect. What it is not is an accident, which is the whole
/// point of writing them down.
const STILL_BARE: &[(&str, &str)] = &[
    (
        "read_image",
        "landed after the last view pass; a picture wants a thumbnail, which \
         needs the fetch-by-id route the attachment note asks for",
    ),
    (
        "search",
        "landed after the last view pass; its results are prose with a snippet \
         per hit and would read far better as rows",
    ),
    (
        "job_list",
        "background work landed after the last view pass; a list of jobs with \
         live state is the one of these three that most wants a view, because \
         a reader watching a build wants it to change without being asked",
    ),
    (
        "job_output",
        "background work landed after the last view pass; its answer is a \
         command's output, which the shell view already knows how to draw - \
         the view is a reuse rather than a new shape",
    ),
    (
        "job_kill",
        "background work landed after the last view pass; it answers one line \
         and the generic frame says it faithfully",
    ),
];

fn app() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/app")
}

/// The tool names the page has an entry for, read from the view tables.
///
/// Only the tables: a key at two spaces inside an `export const … = {` block
/// whose name ends in `Views`, or `tools.js`'s own `export const views = {`.
/// Scanning the files at large picks up unrelated lookup tables - the hook
/// decision words `allow`, `deny`, `pass` all look like tool names - and a
/// check that counts those is a check that passes for the wrong reason.
fn views() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for entry in std::fs::read_dir(app()).expect("web/app is readable") {
        let path = entry.expect("a readable entry").path();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if !(name == "tools.js" || (name.starts_with("tool-") && name.ends_with(".js"))) {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("a readable script");
        let mut inside = false;
        for line in body.lines() {
            if let Some(rest) = line.strip_prefix("export const ") {
                inside = rest.ends_with("Views = {") || rest == "views = {";
                continue;
            }
            if inside {
                if line == "};" {
                    inside = false;
                    continue;
                }
                if let Some(key) = table_key(line) {
                    found.insert(key);
                }
            }
        }
    }
    assert!(
        found.len() > 5,
        "the view tables stopped parsing; found only {found:?}"
    );
    found
}

/// `  name: {` at the table's own indent - opening a block or closing on the
/// same line - or `  name: null,`, which is the table's way of saying a name
/// is known and deliberately undrawn.
fn table_key(line: &str) -> Option<String> {
    let rest = line.strip_prefix("  ")?;
    if rest.starts_with(' ') {
        return None;
    }
    let (name, tail) = rest.split_once(": ")?;
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        || name.is_empty()
    {
        return None;
    }
    let tail = tail.trim();
    (tail.starts_with('{') || tail == "null,").then(|| name.to_string())
}

/// TC-WEB-8: every tool this build offers is either drawn or listed as bare.
#[test]
fn the_panel_accounts_for_every_tool_this_build_offers() {
    let offered: BTreeSet<String> = tetanus_toolset::stock_registry()
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    let drawn = views();
    let bare: BTreeSet<String> = STILL_BARE
        .iter()
        .map(|(name, _)| name.to_string())
        .collect();

    let unaccounted: Vec<&String> = offered
        .iter()
        .filter(|name| !drawn.contains(*name) && !bare.contains(*name))
        .collect();
    assert!(
        unaccounted.is_empty(),
        "these tools are offered and the browser panel says nothing about them - \
         give each a view in web/app/tool-*.js or a line in STILL_BARE saying why \
         not: {unaccounted:?}"
    );

    // The other direction, so the list shrinks when the work is done rather
    // than outliving it.
    let stale: Vec<&String> = bare.iter().filter(|name| drawn.contains(*name)).collect();
    assert!(
        stale.is_empty(),
        "these are listed as bare and now have a view; drop them from \
         STILL_BARE: {stale:?}"
    );

    let gone: Vec<&String> = bare
        .iter()
        .filter(|name| !offered.contains(*name))
        .collect();
    assert!(
        gone.is_empty(),
        "these are listed as bare and this build no longer offers them; drop \
         them from STILL_BARE: {gone:?}"
    );
}

/// TC-WEB-9: the page draws no tool this build does not offer.
///
/// A view for a name nothing registers is dead code that reads as coverage.
/// The exception is a tool a deployment turns on from its settings document -
/// `web_fetch` and `web_search` are real and simply off in the stock
/// composition - so those are allowed and named.
#[test]
fn the_panel_draws_no_tool_that_does_not_exist() {
    const BY_SETTING: &[&str] = &["web_fetch", "web_search"];
    let offered: BTreeSet<String> = tetanus_toolset::stock_registry()
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    let invented: Vec<String> = views()
        .into_iter()
        .filter(|name| !offered.contains(name) && !BY_SETTING.contains(&name.as_str()))
        .collect();
    assert!(
        invented.is_empty(),
        "the page has views for tools nothing registers: {invented:?}"
    );
}
