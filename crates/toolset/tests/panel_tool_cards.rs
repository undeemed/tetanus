//! Which tool cards the ported panel draws with a shape of their own.
//!
//! The sibling of `browser_views.rs`, and here for the same reason that file
//! gives: [`stock_registry`] is in this crate, and the alternative was a test
//! in `crates/host` with a dev-dependency on this one - a crate edge from the
//! thing that serves files to the thing that knows about tools, added for one
//! assertion. Reading a TypeScript file as text adds no edge at all.
//!
//! # Why this is not `browser_views.rs` again
//!
//! `web/app` has a view table of its own, so its risk is a view for a tool
//! nothing registers, and a tool nothing draws. `web/deepseek` has no such
//! table: upstream's row model owns the mapping from a tool name to a card,
//! and everything it does not recognise lands on the generic card, which is
//! honest and complete.
//!
//! What can rot instead is the *overlap* between two independently chosen sets
//! of tool names. Our `shell` is upstream's `bash` and our `search` is their
//! `grep`, so those take the generic card - but `read`, `write`, `edit` and
//! `glob` happen to be spelled the same in both projects and therefore get a
//! shaped one. Nothing in either repository says so, and a tool renamed on
//! either side would change its card silently. This is that sentence, asserted.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// The ported panel's directory, from this crate rather than from the working
/// directory, so the case answers the same wherever it is run from.
fn panel() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/deepseek")
}

fn read(name: &str) -> String {
    let path = panel().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// Every tool name upstream's row model classifies into a visual variant.
///
/// Read from the vendored source rather than copied, so an upstream refresh
/// that adds a name is seen by the next test run rather than by nobody.
fn upstream_variants() -> BTreeSet<String> {
    let body = read("upstream/ui-tool/client/tool/models/tool-call-model.ts");
    let start = body
        .find("const TOOL_VARIANTS")
        .expect("upstream still has its variant table");
    let open = body[start..].find('{').expect("the table opens") + start + 1;
    let end = body[open..].find("\n}").expect("the table closes") + open;
    body[open..end]
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("  ")?;
            if rest.starts_with(' ') {
                return None;
            }
            let (name, _) = rest.split_once(':')?;
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
                .then(|| name.to_string())
        })
        .collect()
}

/// Tools this build offers that upstream's row model already knows how to
/// draw, and therefore draws with a shaped card rather than the generic one.
///
/// The list is short because the two projects named their tools
/// independently: our `shell` is upstream's `bash`, our `search` is their
/// `grep`. Those tools land on the generic card, which is honest and complete;
/// what it is not is an accident, which is the point of writing the overlap
/// down and asserting it in both directions.
const SHAPED: &[&str] = &["edit", "glob", "read", "write"];

/// TC-PANEL-TOOLS-1: the tool cards this build draws are the ones we say they are.
///
/// Answers TC-WEB-8 and TC-WEB-9 together, and it has to be stated differently
/// because the panel has no view table of its own: upstream's row model owns
/// the mapping, so what can rot is not "a view for a tool nothing registers"
/// but the silent overlap between two independently chosen sets of tool names.
/// A tool renamed to `bash` would change its card with nothing in this
/// repository saying so.
///
/// Expected: the intersection of what this build offers and what upstream
/// classifies is exactly [`SHAPED`], and every name in [`SHAPED`] is still a
/// tool this build offers.
#[test]
fn the_tools_that_get_a_shaped_card_are_the_ones_named() {
    let offered = offered_tools();
    let classified = upstream_variants();
    let shaped: BTreeSet<String> = SHAPED.iter().map(|name| name.to_string()).collect();

    let actual: BTreeSet<String> = offered.intersection(&classified).cloned().collect();
    assert_eq!(
        actual, shaped,
        "the set of tools upstream draws with a shaped card has moved. Either \
         a tool was renamed into or out of upstream's table, or upstream's \
         table changed on a refresh - decide which card each should get and \
         update SHAPED"
    );

    let gone: Vec<&String> = shaped
        .iter()
        .filter(|name| !offered.contains(*name))
        .collect();
    assert!(
        gone.is_empty(),
        "these are named as shaped and this build no longer offers them: {gone:?}"
    );
}

/// Every tool this build offers, through the one place that says so.
fn offered_tools() -> BTreeSet<String> {
    // The catalogue the binary composes, not a list re-typed here: `tetanus
    // tools` and what a turn dispatches come from the same source, and a
    // second list would be the thing that drifts.
    tetanus_toolset::stock_registry()
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect()
}
