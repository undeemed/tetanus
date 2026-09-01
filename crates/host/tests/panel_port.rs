//! What holds the ported browser panel together, asserted.
//!
//! `web/deepseek` is DeepSeek Harness's conversation view over our carrier.
//! `web/app` is the panel that has no build step, and `web_app.rs` beside this
//! file is its guard; the two panels are guarded separately because they fail
//! differently, and neither file is the other's successor until the captain
//! says which one ships.
//!
//! # The floor this file has to clear
//!
//! `web_app.rs` fixes fifteen structural properties of a panel. They are not
//! that panel's implementation details - each one is a defect somebody found
//! by hand after it shipped - so a replacement panel satisfies an equivalent
//! or says in writing why not. `data/tetanus-ui-port/report.md` carries the
//! justifications; the equivalences are here, each named with the case it
//! answers.
//!
//! Four of them - a module that does not parse, a name declared twice, an
//! import that resolves to nothing, and a script the page does not actually
//! load - are answered by one thing: the panel is built by a bundler that
//! parses, resolves and links every module, and `TC-PANEL-1` runs that build.
//! That is a **stronger** claim than `node --check` per file, because a file
//! can parse perfectly and import a module that does not exist.
//!
//! # Why the build is inside the merge gate rather than beside it
//!
//! `cargo test --workspace` is what this project calls its gate, and until
//! this file the CI workflow ran `fmt`, `clippy`, `build` and `test` with no
//! Node anywhere - which meant `web_app.rs`'s own parse guard, whose whole
//! reason for existing was a `SyntaxError` that shipped a dead panel through a
//! green build, **skipped on every pull request this project has ever had**.
//! It only ever ran on a developer's machine.
//!
//! So the build is a test, not a workflow step. A developer running the gate
//! gets the answer CI gets, one rule decides what happens when Node is
//! missing, and there is no second place for the guard to be forgotten.
//!
//! # Missing Node is a skip on a laptop and a failure in CI
//!
//! A skip in CI is not protection, it is the absence of protection wearing
//! protection's clothes. `CI` is set by every hosted runner, so its presence
//! is what turns the skip into a failure. On a machine with no Node the case
//! says loudly what it did not check and passes, which keeps the project's
//! "no runtime to install" promise true for anyone building only the binary.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

/// The panel's directory, from this crate rather than from the working
/// directory, so the case answers the same wherever it is run from.
fn panel() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/deepseek")
}

fn read(name: &str) -> String {
    let path = panel().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// Whether this is a hosted runner rather than somebody's machine.
fn hosted() -> bool {
    std::env::var_os("CI").is_some()
}

/// What the toolchain can do here: build, or say why not.
enum Toolchain {
    Ready,
    Missing(String),
}

fn toolchain() -> Toolchain {
    let ran = |program: &str| {
        Command::new(program)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    };
    if !ran("node") {
        return Toolchain::Missing("no `node` on PATH".into());
    }
    if !ran("pnpm") {
        return Toolchain::Missing("no `pnpm` on PATH".into());
    }
    if !panel().join("node_modules").is_dir() {
        return Toolchain::Missing(
            "web/deepseek/node_modules is absent; run `pnpm install` there".into(),
        );
    }
    Toolchain::Ready
}

/// TC-PANEL-1: the panel builds, so every module it ships parses, resolves and
/// links.
///
/// Answers TC-WEB-4 (every import is a file that exists), TC-WEB-7 (the page's
/// modules are the ones it loads), TC-WEB-12 (every script parses) and
/// TC-WEB-13 (no module declares one top-level name twice) with one claim that
/// is stronger than all four: a bundler will not emit a bundle for a tree with
/// a syntax error, an unresolvable import, or a duplicate binding, and it
/// links the graph rather than checking files one at a time.
///
/// Expected: exit status 0 from `pnpm run build`, and a `dist/index.html` on
/// disk afterwards.
#[test]
fn the_panel_builds() {
    match toolchain() {
        Toolchain::Missing(why) => {
            assert!(
                !hosted(),
                "TC-PANEL-1: {why}. In CI that is a failure, not a skip: this \
                 case is the only thing that parses the panel's modules, and a \
                 panel whose modules do not parse is a blank page that ships \
                 through an otherwise green gate. Add Node, pnpm and a `pnpm \
                 install` step to the workflow."
            );
            eprintln!(
                "TC-PANEL-1: {why}, so the panel was NOT built and nothing \
                 here parsed its modules. This is a skip only because this is \
                 not CI."
            );
        }
        Toolchain::Ready => {
            let built = Command::new("pnpm")
                .args(["run", "build"])
                .current_dir(panel())
                .output()
                .expect("pnpm runs");
            assert!(
                built.status.success(),
                "TC-PANEL-1: the panel does not build, so the page it serves \
                 is dead:\n{}\n{}",
                String::from_utf8_lossy(&built.stdout),
                String::from_utf8_lossy(&built.stderr),
            );
            assert!(
                panel().join("dist/index.html").is_file(),
                "TC-PANEL-1: the build reported success and emitted no index"
            );
        }
    }
}

/// TC-PANEL-2: our side of the seam type-checks.
///
/// The vendored tree is deliberately outside the type-check (`tsconfig.json`
/// says why), so this checks the half that is ours: the carrier, the fold, the
/// store and the renderer table. Those are the files that can be wrong in a
/// way upstream's own CI has never seen.
#[test]
fn the_adapter_type_checks() {
    match toolchain() {
        Toolchain::Missing(why) => {
            assert!(!hosted(), "TC-PANEL-2: {why}. See TC-PANEL-1.");
            eprintln!("TC-PANEL-2: {why}, so `src/` was NOT type-checked.");
        }
        Toolchain::Ready => {
            let checked = Command::new("pnpm")
                .args(["run", "check"])
                .current_dir(panel())
                .output()
                .expect("pnpm runs");
            assert!(
                checked.status.success(),
                "TC-PANEL-2: the adapter does not type-check:\n{}\n{}",
                String::from_utf8_lossy(&checked.stdout),
                String::from_utf8_lossy(&checked.stderr),
            );
        }
    }
}

/// Run one of the panel's package scripts under the Node rule.
///
/// Written once because there are four of these and the rule about a missing
/// toolchain has to be the same for all of them: a case that skipped where its
/// neighbour failed would be the gap this whole file exists to close.
fn panel_script(case: &str, script: &str, why_it_matters: &str) {
    if let Toolchain::Missing(why) = toolchain() {
        assert!(
            !hosted(),
            "{case}: {why}. In CI that is a failure, not a skip: {why_it_matters} \
             Add Node, pnpm and a `pnpm install` step to the workflow."
        );
        eprintln!("{case}: {why}, so `pnpm run {script}` did NOT run. Skipped only because this is not CI.");
        return;
    }
    let ran = std::process::Command::new("pnpm")
        .args(["run", script])
        .current_dir(panel())
        .output()
        .expect("pnpm runs");
    assert!(
        ran.status.success(),
        "{case}: `pnpm run {script}` failed:\n{}\n{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr),
    );
}

/// TC-PANEL-3: the adapter's own specs pass, at 100% of every line.
///
/// `cargo llvm-cov` measures Rust and cannot see one line of this panel, so a
/// ported React surface would otherwise land with zero measured coverage -
/// which is exactly the condition that shipped a dead panel through a green
/// gate. The threshold is per file and lives in `vitest.config.ts`; this case
/// is what makes `cargo test --workspace` fail when it is not met.
#[test]
fn the_adapters_specs_hold_every_line() {
    panel_script(
        "TC-PANEL-3",
        "test:coverage",
        "it is the only thing that runs the panel's own tests, and the panel is \
         where this change's logic actually lives.",
    );
}

/// TC-PANEL-4: upstream's own specs still pass against the vendored copy.
///
/// 26 spec files and 521 cases came across with the components. They are not
/// held to a coverage threshold - upstream does not hold this code to one
/// either, and `vitest.config.ts` says why - but they must pass, because they
/// are the only thing that would notice a refresh quietly changing behaviour
/// in 8,000 lines nobody here wrote.
#[test]
fn upstreams_own_specs_still_pass() {
    panel_script(
        "TC-PANEL-4",
        "test:upstream",
        "it is the only check on the 8,000 vendored lines this panel draws with.",
    );
}

/// TC-PANEL-5: every id a script reaches for is an id the page has.
///
/// Answers TC-WEB-3 unchanged, and the panel makes it nearly trivial - one
/// mount point instead of a page full of named seats - which is worth keeping
/// rather than dropping, because the one id is the one that turns the whole
/// page blank when it is renamed.
#[test]
fn every_id_a_script_reaches_for_is_on_the_page() {
    let page = read("index.html");
    let ids: BTreeSet<String> = quoted_after(&page, "id=").into_iter().collect();
    let mut reached = BTreeSet::new();
    for (name, body) in sources() {
        for id in quoted_after(&body, "getElementById(") {
            reached.insert((name.clone(), id));
        }
    }
    let missing: Vec<&(String, String)> =
        reached.iter().filter(|(_, id)| !ids.contains(id)).collect();
    assert!(
        missing.is_empty(),
        "these ids are reached for and the page has none of them: {missing:?}"
    );
}

/// TC-PANEL-6: no stylesheet the panel adds can collide with another.
///
/// TC-WEB-1 and TC-WEB-2 exist because a stylesheet has one namespace and no
/// compiler, so `.row` defined twice is silent and order-dependent. This stack
/// answers that by construction rather than by inspection: every component
/// sheet is a CSS module compiled to a hashed name, so two files exporting
/// `.row` produce two different class names and no element can wear one from
/// each.
///
/// That is only true while it stays true, which is what this case is for. It
/// asserts the mechanism is switched on - the hash is in the generated name -
/// and that no component imports a plain global stylesheet, which would be the
/// one way back into the old failure.
#[test]
fn every_component_stylesheet_is_scoped() {
    let config = read("vite.config.ts");
    assert!(
        config.contains("generateScopedName") && config.contains("[hash:"),
        "the CSS-module name pattern lost its hash, so two files exporting one \
         class name now collide exactly as they did in web/app"
    );

    // The global sheets are upstream's theme, and a theme is allowed to be
    // global - what it is not allowed to do is carry class rules, which is
    // what would collide. `--dsw-*` custom properties on `:root`/`body` cannot.
    let mut offending = Vec::new();
    for (name, body) in sources() {
        if !name.ends_with(".css") || name.contains(".module.") {
            continue;
        }
        for line in body.lines() {
            let line = line.trim();
            // A selector line, not a declaration and not a comment.
            if line.starts_with('.') && line.contains('{') {
                offending.push(format!("{name}: {line}"));
            }
        }
    }
    assert!(
        offending.is_empty(),
        "these global stylesheets declare class rules, which is the unscoped \
         namespace TC-WEB-1 and TC-WEB-2 were written about: {offending:?}"
    );
}

/// TC-PANEL-7: nothing the panel adds is set as markup.
///
/// Answers TC-WEB-5. Scoped to our own source rather than the whole tree,
/// because upstream has exactly one such site and it is a considered one:
/// `CodeBlock.tsx` renders the HTML a syntax highlighter produced, which is
/// the sanctioned path for that library and is not user input. Widening the
/// rule to the vendored tree would mean either a false failure or an exception
/// list that reads as permission.
#[test]
fn nothing_the_panel_adds_is_set_as_markup() {
    let mut found = Vec::new();
    for (name, body) in ours() {
        // The product, not its tests: a spec that clears `document.body` is
        // arranging a fixture, and the rule is about what a transcript does
        // with what a tool printed.
        if !name.starts_with("src/") {
            continue;
        }
        if body.contains("dangerouslySetInnerHTML") || body.contains(".innerHTML") {
            found.push(name);
        }
    }
    assert!(
        found.is_empty(),
        "these set markup rather than text, which is how a transcript renders \
         what a tool printed as HTML: {found:?}"
    );
}

/// TC-PANEL-8: nothing the panel adds pins a width a narrow screen cannot give.
///
/// Answers TC-WEB-6. A fixed `width: NNNpx` on a flex child is the shape, and
/// so is a flex child with no `min-width: 0`, which is what makes a long
/// unbroken string push its neighbours off the bar.
#[test]
fn nothing_the_panel_adds_pins_a_width() {
    let mut pinned = Vec::new();
    for (name, body) in ours() {
        if !name.ends_with(".css") {
            continue;
        }
        for line in body.lines() {
            let line = line.trim();
            let Some(value) = line.strip_prefix("width:") else {
                continue;
            };
            let value = value.trim().trim_end_matches(';');
            // A minimum, a maximum, a percentage, a viewport unit and a
            // `min()` all yield to a small screen. A bare pixel width does not.
            if value.ends_with("px") {
                pinned.push(format!("{name}: {line}"));
            }
        }
    }
    assert!(
        pinned.is_empty(),
        "these pin a width a narrow screen cannot give: {pinned:?}"
    );
}

/// Durable types the fold draws through the raw fallback rather than a shaped
/// row.
///
/// Being here is a decision, not a defect: contract §4.3.2 says a surface
/// passes an unknown type through, and the fold's `unknown` node does exactly
/// that - the reader sees a labelled JSON card rather than nothing. What it is
/// not is an accident, which is the point of writing them down.
const UNDRAWN: &[(&str, &str)] = &[
    (
        "approval/asked",
        "an approval is a wait, and a wait needs the composer to hand over to \
         it - which is upstream's ApprovalPanel and is staged behind the \
         composer port",
    ),
    ("approval/decided", "as above"),
    ("approval/policy", "as above"),
    (
        "question/asked",
        "the same shape as an approval and blocked on the same seat",
    ),
    ("question/answered", "as above"),
    (
        "compaction/start",
        "upstream draws a compaction as one marker built from the summary, the \
         prune and the replacement message together; the pieces render raw \
         until the whole transaction is folded",
    ),
    ("compaction/end", "as above"),
    ("compaction/summary", "as above"),
    ("compaction/prune", "as above"),
    (
        "context/snapshot",
        "the live facts a turn told the model; upstream has a context meter for \
         this and it belongs with the header, not in the flow",
    ),
    (
        "request/context",
        "written once per step, so it is the most common raw row on the screen \
         and the most worth folding into the step it belongs to",
    ),
    (
        "llm/retry",
        "upstream folds a retry chain into one row through its `model-retry` \
         node; the fold has to correlate the attempts first",
    ),
    ("llm/retry-started", "as above"),
    (
        "goal/changed",
        "the goal family belongs to a surface this screen does not have",
    ),
    ("plan/mode", "the plan family, as above"),
    ("plan/presented", "as above"),
    (
        "todo/write",
        "upstream's TodoPanel owns this, and it sits in the shell",
    ),
    (
        "attachment/added",
        "needs the fetch-by-id route an image view would use",
    ),
    (
        "feedback/recorded",
        "log-only intent; it has no row anywhere yet",
    ),
    (
        "fs/mode",
        "a session-scoped knob whose place is a pill in the header",
    ),
    ("permission/preset", "as above"),
    (
        "hook/invoked",
        "a hook decision is audit rather than conversation, and belongs beside \
         the approval audit when that surface exists",
    ),
    ("hook/result", "as above"),
    ("subagent/descriptor", "a subagent needs a pane, not a row"),
    (
        "workflow/start",
        "four types that want one view between them, showing a run's steps as \
         they settle",
    ),
    ("workflow/step-start", "as above"),
    ("workflow/step-end", "as above"),
    ("workflow/end", "as above"),
];

/// TC-PANEL-9: every durable type the engine writes is drawn or listed.
///
/// Answers TC-WEB-10. The fold declares what it draws in one exported array,
/// so this reads a list rather than scraping string literals out of scripts -
/// which makes it both stricter and quieter than the case it replaces.
#[test]
fn the_panel_accounts_for_every_durable_type_the_engine_writes() {
    let drawn = drawn_types();
    let listed: BTreeSet<String> = UNDRAWN.iter().map(|(ty, _)| ty.to_string()).collect();
    let written = durable_topics();

    let unaccounted: Vec<&String> = written
        .iter()
        .filter(|ty| !drawn.contains(*ty) && !listed.contains(*ty))
        .collect();
    assert!(
        unaccounted.is_empty(),
        "the engine writes these and the panel says nothing about them - fold \
         them in web/deepseek/src/timeline.ts, or add a line to UNDRAWN saying \
         why not: {unaccounted:?}"
    );

    let stale: Vec<&String> = listed.iter().filter(|ty| drawn.contains(*ty)).collect();
    assert!(
        stale.is_empty(),
        "these are listed as undrawn and the fold now draws them; drop them \
         from UNDRAWN: {stale:?}"
    );

    let gone: Vec<&String> = listed.iter().filter(|ty| !written.contains(*ty)).collect();
    assert!(
        gone.is_empty(),
        "these are listed as undrawn and nothing writes them any more; drop \
         them from UNDRAWN: {gone:?}"
    );
}

/// TC-PANEL-10: the panel folds no event the engine cannot produce.
///
/// Answers TC-WEB-11, and guards the same traceless failure: a mistyped `case`
/// never matches, the event falls to the raw path, and the transcript looks
/// like one the engine never mentioned it. Nothing throws and nothing logs.
#[test]
fn the_panel_folds_no_event_the_engine_cannot_produce() {
    let written = durable_topics();
    let invented: Vec<String> = drawn_types()
        .into_iter()
        .filter(|ty| !written.contains(ty))
        .collect();
    assert!(
        invented.is_empty(),
        "the fold names these and the engine writes no such thing - a name \
         that never matches is a branch that never runs: {invented:?}"
    );
}

/// TC-PANEL-11: the attribution travels with the copy.
///
/// Not one of the fifteen, and the one failure the brief calls unrecoverable
/// by a later commit. Every vendored file carries upstream's copyright and
/// licence, upstream's own `LICENSE` is present verbatim, and a file that was
/// modified says so in its own header rather than claiming to be a verbatim
/// copy.
#[test]
fn every_vendored_file_carries_its_notice() {
    let root = panel().join("upstream");
    assert!(
        root.join("LICENSE").is_file(),
        "upstream's LICENSE is not beside the code it licenses"
    );
    let licence = std::fs::read_to_string(root.join("LICENSE")).expect("a readable licence");
    assert!(
        licence.contains("MIT License") && licence.contains("DeepSeek"),
        "the vendored LICENSE is not the notice the copies claim"
    );

    let mut bare = Vec::new();
    let mut lying = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a readable directory") {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            // Source carries a header; data cannot. A comment inside a
            // snapshot golden IS the golden, so those are covered by
            // `upstream/LICENSE` beside them and by `NOTICE.md`, which is what
            // the MIT licence asks for a copy that has nowhere to put a
            // header. `SPECS-NOT-PORTED.txt` is ours, not upstream's.
            let source = path
                .extension()
                .and_then(|end| end.to_str())
                .is_some_and(|end| matches!(end, "ts" | "tsx" | "css"));
            if !source {
                continue;
            }
            let body = std::fs::read_to_string(&path).expect("a readable file");
            let named = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .display()
                .to_string();
            let head: String = body.lines().take(12).collect::<Vec<_>>().join("\n");
            if !head.contains("Copyright (c) 2026 DeepSeek")
                || !head.contains("MIT License")
                || !head.contains("deepseek-ai/deepseek-harness")
            {
                bare.push(named.clone());
                continue;
            }
            // A copy that was edited must not go on claiming it was not.
            let says_verbatim = head.contains("Unmodified");
            let says_modified = head.contains("MODIFIED by the tetanus project");
            if says_verbatim == says_modified {
                lying.push(named);
            }
        }
    }
    assert!(
        bare.is_empty(),
        "these vendored files carry no upstream copyright notice, which is the \
         one thing the MIT licence asks of a copy: {bare:?}"
    );
    assert!(
        lying.is_empty(),
        "these vendored files claim both or neither of verbatim and modified; \
         each header must say exactly one: {lying:?}"
    );
}

/// TC-PANEL-12: upstream's brand art is not vendored.
///
/// The MIT licence grants copyright permission and says nothing about trade
/// marks. DeepSeek's whale mark and its `deepseek-official HARNESS`
/// letterforms are trade marks, so they must not ship inside a product called
/// something else - which is a different rule from the rebranding of product
/// strings, and the one that a rebrand pass would most easily get wrong by
/// keeping the art and changing the words.
#[test]
fn upstream_brand_art_is_not_vendored() {
    let root = panel().join("upstream");
    for name in [
        "ui-primitives/BrandWordmark.tsx",
        "ui-primitives/FishLogo.tsx",
    ] {
        assert!(
            !root.join(name).exists(),
            "{name} is DeepSeek brand art and is not ours to ship under \
             another name; `tools/vendor.py` refuses it"
        );
    }
    let barrel = read("upstream/ui-primitives/index.ts");
    assert!(
        !barrel.contains("BrandWordmark") || barrel.contains("MODIFIED"),
        "the primitives barrel re-exports brand art again"
    );
}

/// Every `.ts`, `.tsx` and `.css` file the panel owns - ours and vendored.
fn sources() -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut stack = vec![panel()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                if path
                    .file_name()
                    .is_some_and(|name| name == "node_modules" || name == "dist")
                {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let is_source = path
                .extension()
                .and_then(|end| end.to_str())
                .is_some_and(|end| matches!(end, "ts" | "tsx" | "css" | "html"));
            if !is_source {
                continue;
            }
            let named = path
                .strip_prefix(panel())
                .unwrap_or(&path)
                .display()
                .to_string();
            if let Ok(body) = std::fs::read_to_string(&path) {
                found.push((named, body));
            }
        }
    }
    found.sort();
    assert!(
        found.len() > 50,
        "the panel lost its sources: {}",
        found.len()
    );
    found
}

/// Only the files this project wrote, which is where our own rules apply.
fn ours() -> Vec<(String, String)> {
    sources()
        .into_iter()
        .filter(|(name, _)| !name.starts_with("upstream/"))
        .collect()
}

/// The durable types the fold draws, read from its own exported list.
fn drawn_types() -> BTreeSet<String> {
    let body = read("src/timeline.ts");
    let start = body
        .find("export const KNOWN")
        .expect("the fold still declares what it draws");
    let open = body[start..].find("= [").expect("the list opens") + start;
    let end = body[open..].find(']').expect("the list closes") + open;
    body[open..end]
        .split('\'')
        .filter(|piece| piece.contains('/') && !piece.contains(','))
        .map(|piece| piece.to_string())
        .collect()
}

/// The durable event types the engine writes.
///
/// The same two conventions `web_app.rs` reads, for the same reason: a
/// constant inside a `mod topic` block is the declared form, and a string
/// literal handed straight to `append` is the other.
fn durable_topics() -> BTreeSet<String> {
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates");
    let mut found = BTreeSet::new();
    let mut stack = vec![crates];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "tests") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|end| end == "rs") {
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                collect_topics(&src, &mut found);
            }
        }
    }
    for line in known_event_renames().lines() {
        if let Some(topic) = topic_literal(line) {
            found.insert(topic);
        }
    }
    assert!(
        found.len() > 20,
        "the topic scan stopped finding anything: {found:?}"
    );
    found
}

/// The `#[serde(rename = "...")]` lines of the contract's `KnownEvent`.
///
/// A third convention beside `mod topic` and a literal `.append`, and the only
/// one that names `session/start` - the journal's first line, written by the
/// session crate through neither of the others. A scan that read only two of
/// the three reported it as a name the engine cannot produce, which is a case
/// failing for a reason that has nothing to do with the panel.
fn known_event_renames() -> String {
    let types = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../protocol/src/types.rs");
    let src = std::fs::read_to_string(&types).expect("the protocol's types are readable");
    src.lines()
        .filter(|line| line.contains("serde(rename = \""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every durable type one Rust source declares or writes.
///
/// Three conventions, because the codebase uses three and a scan that saw two
/// of them reports a blind spot as a clean bill of health.
///
/// - a constant inside a `mod topic` block, which is the declared form;
/// - a literal handed straight to `append`;
/// - a constant whose NAME ends in `_EVENT`, which is how `llm/retry`,
///   `llm/retry-started` and `subagent/descriptor` are declared. `web_app.rs`
///   reads only the first two, so it calls those three unwritten while the
///   engine writes them.
///
/// The third rule is on the constant's name rather than on its value on
/// purpose. Sweeping every `const` holding a slash string instead picks up
/// twenty-two things that are not journal types - plugin hook points like
/// `agent/pre-step`, push names like `session/event`, and the media type
/// `application/json` - and a set that large is a case that fails for reasons
/// having nothing to do with the panel.
fn collect_topics(src: &str, found: &mut BTreeSet<String>) {
    for (at, _) in src.match_indices("mod topic") {
        let Some(open) = src[at..].find('{').map(|index| at + index + 1) else {
            continue;
        };
        let mut depth = 1usize;
        let mut end = open;
        for (offset, ch) in src[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        for line in src[open..end].lines() {
            if line.contains("pub const") {
                if let Some(topic) = topic_literal(line) {
                    found.insert(topic);
                }
            }
        }
    }
    for (at, _) in src.match_indices(".append") {
        let head: String = src[at..].chars().take(60).collect();
        if let Some(topic) = topic_literal(&head) {
            found.insert(topic);
        }
    }
    // The method and push tables are cut out first: `SESSION_EVENT`
    // there is the push named `session/event`, which is a frame the server
    // sends rather than a line it writes, and demanding the panel draw it
    // would be demanding it draw the envelope its own events arrive in.
    for line in without_blocks(src, &["mod method", "mod push"]).lines() {
        let Some((declaration, _)) = line.split_once(": &str = ") else {
            continue;
        };
        if !declaration.trim_end().ends_with("_EVENT") {
            continue;
        }
        if let Some(topic) = topic_literal(line) {
            found.insert(topic);
        }
    }
}

/// One source with the named `mod` blocks removed, braces balanced.
fn without_blocks(src: &str, names: &[&str]) -> String {
    let mut out = src.to_string();
    for name in names {
        while let Some(at) = out.find(name) {
            let Some(open) = out[at..].find('{').map(|index| at + index + 1) else {
                break;
            };
            let mut depth = 1usize;
            let mut end = out.len();
            for (offset, ch) in out[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + offset + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.replace_range(at..end, "");
        }
    }
    out
}

/// The first `"family/name"` in a line, and nothing that is not one.
fn topic_literal(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = start + line[start..].find('"')?;
    let text = &line[start..end];
    let (family, name) = text.split_once('/')?;
    let ok = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit())
    };
    (ok(family) && ok(name)).then(|| text.to_string())
}

/// Every string literal that follows `marker`, in either quote style.
fn quoted_after(text: &str, marker: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (at, _) in text.match_indices(marker) {
        let rest = &text[at + marker.len()..];
        let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        let body = &rest[quote.len_utf8()..];
        if let Some(end) = body.find(quote) {
            found.push(body[..end].to_string());
        }
    }
    found
}
