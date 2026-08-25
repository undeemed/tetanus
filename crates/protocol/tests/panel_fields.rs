//! Test Design Specification: the browser panel reads fields this contract names.
//!
//! Feature under test: the agreement between `web/app`'s reads off
//! `SessionEvent.data` and the payload tables of
//! `docs/interface-contract.md` sections 4.3.1 and 4.3.2.
//!
//! Why this case exists, and why it is here rather than in a suite of its own.
//! The presentation lane wrote down the risk it cannot defend against from its
//! own side: "a field this page reads that is later renamed fails silently,
//! drawing an empty panel rather than a build error". JavaScript has no build
//! to break. Rust has one, and this crate is where the boundary's shapes live,
//! so the check belongs on this side of the seam - it costs the panel nothing
//! and fails the engine's build if the engine renames a field somebody draws.
//!
//! What this does NOT check, deliberately. It does not run the panel, render
//! anything, or assert that a field carries a sensible value: that is a
//! browser's job and this is a text comparison. It proves one thing - every
//! name the panel destructures is a name this contract publishes - which is
//! exactly the failure mode that has no other detector.
//!
//! Environmental needs: the repository checkout. No network, no browser, no
//! runtime.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The repository root, from this crate's manifest.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/protocol is two levels under the root")
        .to_path_buf()
}

/// Every `data.<field>` the panel reads, and which module reads it.
///
/// A deliberately blunt scan: the panel is JavaScript and this is not a
/// parser. A blunt scan errs by finding *more* names than the panel truly
/// depends on, which is the safe direction - a name that is checked and did
/// not need to be costs nothing, while a name that is missed is the silent
/// failure this exists to catch.
fn panel_reads() -> BTreeMap<String, BTreeSet<String>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let dir = root().join("web/app");
    let mut modules: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the panel is in the tree")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "js"))
        .collect();
    modules.sort();
    assert!(!modules.is_empty(), "the panel has modules to read");

    for path in modules {
        let name = path
            .file_name()
            .expect("a file name")
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(&path).expect("the module reads");
        for field in fields_of(&text) {
            found.entry(field).or_default().insert(name.clone());
        }
    }
    found
}

/// `data.<name>` occurrences, with `<name>` taken whole.
///
/// Whole, including its capitals: the first cut of this scan matched
/// `[a-z_]+` and truncated `data.handlerId` to `handler`, then reported four
/// non-existent fields as missing. The bug was in the probe and not in the
/// panel, which is the failure mode a checker of names has to be built against
/// - a wrong probe accuses the wrong side, convincingly.
fn fields_of(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut at = 0usize;
    while let Some(hit) = text[at..].find("data.") {
        let start = at + hit;
        // `data.` has to start a word, or `metadata.foo` reads as a field.
        let preceded_by_word = start > 0 && {
            let previous = bytes[start - 1];
            previous.is_ascii_alphanumeric() || previous == b'_'
        };
        at = start + "data.".len();
        if preceded_by_word {
            continue;
        }
        let name: String = text[at..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.insert(name);
        }
    }
    out
}

/// TC-PANEL-FIELD-1: every field the browser panel reads off an event is a
/// field this contract publishes.
///
/// Input: the `data.<field>` names in `web/app/*.js`, and the text of
/// `docs/interface-contract.md`.
/// Expected: every name appears in the document. A rename on the engine side
/// therefore fails this build, which is the only place it can fail - the panel
/// would draw an empty pane and say nothing.
#[test]
fn every_field_the_panel_reads_is_named_in_the_contract() {
    let contract = std::fs::read_to_string(root().join("docs/interface-contract.md"))
        .expect("the contract is in the tree");
    let reads = panel_reads();
    assert!(
        reads.len() > 20,
        "the scan found {} fields, which is too few to be a real read of the panel",
        reads.len()
    );

    let unnamed: Vec<String> = reads
        .iter()
        .filter(|(field, _)| !contract.contains(&format!("`{field}`")))
        .map(|(field, modules)| {
            let modules: Vec<&str> = modules.iter().map(String::as_str).collect();
            format!("{field} (read by {})", modules.join(", "))
        })
        .collect();

    assert!(
        unnamed.is_empty(),
        "the panel reads {} field(s) this contract does not name:\n  {}\n\
         Either the engine renamed a published field - fix the contract and the panel together, \
         per section 5 - or the panel is reading something that was never promised.",
        unnamed.len(),
        unnamed.join("\n  ")
    );
}

/// TC-PANEL-FIELD-2: the scan reads whole field names, capitals included.
///
/// The case that exists because the first cut of this file got it wrong. A
/// name-checker that truncates names accuses the other side of a defect it
/// does not have, and does it with a specific, plausible list.
///
/// Input: a line reading a camel-cased field, and one where `data.` is the
/// tail of a longer word.
/// Expected: the camel-cased name whole; nothing from `metadata.`.
#[test]
fn the_scan_takes_a_field_name_whole_and_ignores_a_word_that_merely_ends_in_data() {
    let camel = fields_of("if (typeof data.durationMs === \"number\") say(data.handlerId);");
    let tail = fields_of("const x = metadata.point; render(data.decision);");

    assert_eq!(
        camel,
        ["durationMs", "handlerId"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );
    assert_eq!(tail, ["decision"].iter().map(|s| s.to_string()).collect());
}
