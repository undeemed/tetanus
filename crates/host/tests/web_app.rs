//! What holds the browser panel together, asserted.
//!
//! `web/app` is the page `crates/host` serves under `tetanus serve --frontend`,
//! and until this file it had **no automated case anywhere in the tree**. Every
//! change to it was defended by somebody looking at it, which is how three
//! separate defects lived there for weeks:
//!
//! - `.row` was defined twice, so every line of the conversation lost its
//!   `display:flex` and the speaker column collapsed into the message;
//! - `.said` was defined by both stylesheets, so every message drew in a
//!   proportional font inside a monospace row;
//! - `.choice.dot` wore one class from each file and stayed correct only for
//!   as long as the load order happened to favour it.
//!
//! Each is the same kind of fault: a stylesheet has one namespace and no
//! compiler, so a name that means two things is silent, order-dependent, and
//! lands on whichever surface loses.
//!
//! # Why these cases and not rendering ones
//!
//! Everything here is a **structural** property that can be read off the files
//! as text. None of it needs a DOM, a browser, or Node - which matters twice
//! over: the CI job is `cargo fmt`, `clippy`, `build`, `test` and nothing else,
//! and the project's own design note says "one self-contained binary, no Node,
//! no `node_modules`, no runtime to install". A test that dragged a browser
//! stack in to check a page would be a bigger claim on this repository than the
//! page itself makes.
//!
//! What that leaves bare is said out loud in the module's last section rather
//! than left for a reader to discover.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

/// The page's directory, from this crate rather than from the process's
/// working directory, so the case answers the same wherever it is run from.
fn app() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/app")
}

fn read(name: &str) -> String {
    let path = app().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// Every script the page ships, so a new module is covered the day it is added
/// rather than the day somebody remembers to list it here.
fn scripts() -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = std::fs::read_dir(app())
        .expect("web/app is readable")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "js"))
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let body = std::fs::read_to_string(entry.path()).expect("a readable script");
            (name, body)
        })
        .collect();
    found.sort();
    assert!(found.len() > 5, "web/app lost its scripts: {found:?}");
    found
}

/// The text inside the page's own `<style>` block.
fn page_style(page: &str) -> &str {
    let start = page.find("<style>").expect("the page has a style block") + "<style>".len();
    let end = page[start..]
        .find("</style>")
        .expect("the style block closes")
        + start;
    &page[start..end]
}

/// Strip `/* ... */`, so a class named only in prose is not read as a rule.
fn without_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(at) = rest.find("/*") {
        out.push_str(&rest[..at]);
        match rest[at..].find("*/") {
            Some(end) => rest = &rest[at + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The classes a stylesheet defines *bare*: a rule whose whole selector is one
/// class, optionally with a pseudo-class.
///
/// Bare is the shape that matters. A rule that scopes a primitive to a context
/// on purpose - `.row > .disclose` - is the cascade being used rather than a
/// second definition of the same name, and refusing it would refuse the
/// mechanism instead of the mistake.
fn bare_classes(css: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for rule in without_comments(css).split('}') {
        let Some((selector, _)) = rule.split_once('{') else {
            continue;
        };
        for one in selector.split(',') {
            let one = one.trim();
            let Some(name) = one.strip_prefix('.') else {
                continue;
            };
            // A pseudo-class is still the same element; anything else - a
            // space, another dot, a combinator - makes it a compound selector.
            let name = name.split(':').next().unwrap_or_default();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                let compound = one.trim_start_matches('.').trim_start_matches(name);
                if compound.is_empty() || compound.starts_with(':') {
                    found.insert(name.to_string());
                }
            }
        }
    }
    found
}

/// TC-WEB-1: one class name has one owner.
///
/// The general form of the `.row`, `.said` and `.choice.dot` defects, stated
/// over the *scripts* rather than the stylesheets because that is where the
/// ownership lives. `primitives.js` is the design system: the classes it puts
/// on elements are its own. A second script putting one of those names on a
/// different element is the clash - two creators for one name - and from then
/// on which rule applies is decided by load order.
///
/// It is deliberately not "no class appears in both stylesheets". The page
/// scoping a primitive on purpose, `.row > .disclose`, has the same shape as
/// the `.said` collision and the opposite meaning; the difference is whether
/// the element came from the primitive, and that is visible here and nowhere
/// else.
#[test]
fn no_class_is_owned_by_the_primitives_and_by_another_script() {
    let mut owners: HashMap<String, BTreeSet<String>> = HashMap::new();
    for (name, body) in scripts() {
        for value in quoted_after(&body, "className = ")
            .into_iter()
            .chain(class_arguments(&body))
        {
            for word in value.split_whitespace() {
                owners
                    .entry(word.to_string())
                    .or_default()
                    .insert(name.clone());
            }
        }
    }
    let mut clashes: Vec<String> = owners
        .iter()
        .filter(|(_, who)| who.contains("primitives.js") && who.len() > 1)
        .map(|(class, who)| format!("{class} set by {who:?}"))
        .collect();
    clashes.sort();
    assert!(
        clashes.is_empty(),
        "these class names have two creators, so which rule applies is decided \
         by load order: {clashes:?}"
    );
}

/// The class argument of `primitives.js`'s own element builder, which sets a
/// class without ever writing `className =`.
fn class_arguments(body: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (at, _) in body.match_indices("make(") {
        let rest = &body[at + "make(".len()..];
        if let Some(class) = quoted_after(rest, ", ").first() {
            found.push(class.clone());
        }
    }
    found
}

/// TC-WEB-2: and no element wears one name from each stylesheet.
///
/// The narrower half, kept because it catches the case where the two names are
/// different but land on one element anyway - `class="choice dot"`, where
/// `choice` is the page's and `dot` is a primitive's.
#[test]
fn no_element_wears_a_page_class_and_a_primitives_class() {
    let page = read("index.html");
    let mine = bare_classes(page_style(&page));
    let theirs = bare_classes(&read("primitives.css"));
    let mut mixed = Vec::new();
    for (name, body) in scripts() {
        for value in quoted_after(&body, "className = ") {
            let words: Vec<&str> = value.split_whitespace().collect();
            if words.iter().any(|w| mine.contains(*w)) && words.iter().any(|w| theirs.contains(*w))
            {
                mixed.push(format!("{name}: {value:?}"));
            }
        }
    }
    assert!(
        mixed.is_empty(),
        "these elements wear a class from each stylesheet: {mixed:?}"
    );
}

/// TC-WEB-3: every id a script reaches for is an id the page has.
///
/// `getElementById` answers `null` and the line after it throws, or an optional
/// chain swallows it and a control simply never works. Nothing in a browser
/// objects, and nothing here did either.
#[test]
fn every_id_a_script_asks_for_exists_in_the_page() {
    let page = read("index.html");
    let ids: BTreeSet<String> = quoted_after(&page, "id=").into_iter().collect();
    let mut absent: Vec<String> = Vec::new();
    for (name, body) in scripts() {
        for wanted in quoted_after(&body, "getElementById(") {
            if !ids.contains(&wanted) {
                absent.push(format!("{name}: #{wanted}"));
            }
        }
    }
    absent.sort();
    absent.dedup();
    assert!(
        absent.is_empty(),
        "these ids are asked for and are not in the page: {absent:?}"
    );
}

/// TC-WEB-4: every module the page imports is a file that exists.
///
/// A mistyped import path is a blank page and a console message nobody sees,
/// and it is the one failure that takes the whole surface rather than a corner
/// of it.
#[test]
fn every_import_and_asset_resolves() {
    let mut missing = Vec::new();
    let page = read("index.html");
    for asset in quoted_after(&page, "src=")
        .into_iter()
        .chain(quoted_after(&page, "href="))
    {
        if asset.starts_with("http") || asset.starts_with('#') {
            continue;
        }
        if !app().join(&asset).exists() {
            missing.push(format!("index.html -> {asset}"));
        }
    }
    for (name, body) in scripts() {
        for target in quoted_after(&body, "from ")
            .into_iter()
            .chain(quoted_after(&body, "import("))
        {
            if !target.starts_with("./") {
                continue;
            }
            if !app().join(target.trim_start_matches("./")).exists() {
                missing.push(format!("{name} -> {target}"));
            }
        }
    }
    assert!(missing.is_empty(), "these do not resolve: {missing:?}");
}

/// TC-WEB-5: nothing on the page is set as markup.
///
/// Everything this page draws comes from a model, a tool or a filesystem, and
/// exactly one of those is trusted. The rule is stated at the top of
/// `primitives.js` and was, until now, enforced by nobody.
#[test]
fn nothing_sets_inner_html() {
    let mut offenders = Vec::new();
    for (name, body) in scripts() {
        for (number, line) in body.lines().enumerate() {
            // A mention in a comment saying "not this" is the point, so the
            // test is for a use: an assignment, or a read.
            let code = line.split("//").next().unwrap_or_default();
            if code.contains("innerHTML") {
                offenders.push(format!("{name}:{}", number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these set or read innerHTML: {offenders:?}"
    );
}

/// TC-WEB-6: nothing pins a width a narrow screen cannot give.
///
/// A fixed `width` or `min-width` in pixels is what makes a column refuse to
/// shrink, and it is the one thing a stylesheet can do that no amount of
/// wrapping recovers from. Under a couple of dozen pixels it is a glyph, not a
/// column - the state dot is 8px and has to be, since a round mark that shrinks
/// is a smudge - so the threshold is the line between the two rather than a
/// list of exceptions: a new small mark needs no ceremony and a new 200px
/// column is still caught.
#[test]
fn no_rule_pins_a_width_a_narrow_screen_cannot_give() {
    const GLYPH: u32 = 24;
    let page = read("index.html");
    let mut pinned = Vec::new();
    for (where_, css) in [
        ("index.html", page_style(&page)),
        ("primitives.css", &read("primitives.css")),
    ] {
        for (property, value) in pixel_widths(css) {
            if value >= GLYPH {
                pinned.push(format!("{where_}: {property}:{value}px"));
            }
        }
    }
    assert!(
        pinned.is_empty(),
        "these pin a width a narrow screen cannot give: {pinned:?}"
    );
}

/// Every `width:` or `min-width:` in pixels, with `max-width` left alone -
/// that one is a ceiling, not a floor.
fn pixel_widths(css: &str) -> Vec<(String, u32)> {
    let mut found = Vec::new();
    let text = without_comments(css);
    for (at, _) in text.match_indices("width:") {
        let before = &text[..at];
        if before.ends_with("max-") || before.ends_with("line-") {
            continue;
        }
        let property = if before.ends_with("min-") {
            "min-width"
        } else {
            "width"
        };
        let rest = text[at + "width:".len()..].trim_start();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && rest[digits.len()..].starts_with("px") {
            if let Ok(value) = digits.parse() {
                found.push((property.to_string(), value));
            }
        }
    }
    found
}

/// TC-WEB-7: the page's modules are the ones the page loads.
///
/// A module nobody imports is dead weight a reader has to decide about, and the
/// count is the cheapest way to notice one arriving.
#[test]
fn every_script_is_reachable_from_the_page() {
    let page = read("index.html");
    let mut imported: HashMap<String, bool> = scripts()
        .into_iter()
        .map(|(name, _)| (name, false))
        .collect();
    let mut reach = |target: &str| {
        if let Some(entry) = imported.get_mut(target.trim_start_matches("./")) {
            *entry = true;
        }
    };
    for asset in quoted_after(&page, "src=") {
        reach(&asset);
    }
    for (_, body) in scripts() {
        for target in quoted_after(&body, "from ")
            .into_iter()
            .chain(quoted_after(&body, "import("))
        {
            reach(&target);
        }
    }
    let orphans: Vec<&String> = imported
        .iter()
        .filter(|(_, seen)| !**seen)
        .map(|(name, _)| name)
        .collect();
    assert!(
        orphans.is_empty(),
        "these scripts are in web/app and nothing loads them: {orphans:?}"
    );
}

/// TC-WEB-14: the picker starts what it picked.
///
/// The catalogue's "Start here" writes `provider` and `model` into the query
/// and reloads. Until this case, nothing read them back: `session.create` was
/// sent `{}` for a fresh session, so every conversation opened on the server's
/// default however the reader had chosen, and the picker was a control that
/// looked like it worked.
///
/// Stated over the text of the two scripts, which is what this file can read.
/// What it cannot see is a browser actually doing it - that is the VNC pass -
/// so the assertion is deliberately about the two halves matching: whatever
/// name the catalogue puts in the query, the page must take out of it and put
/// in the call.
#[test]
fn the_page_passes_the_picked_provider_and_model_into_session_create() {
    let chat = read("chat.js");
    let catalogue = read("catalogue.js");

    // The catalogue is what names the two parameters, and `chat.js` writes
    // them; both halves of the round trip live in this repository, so the
    // names are checked against each other rather than against a constant.
    for param in ["provider", "model"] {
        assert!(
            chat.contains(&format!("searchParams.set(\"{param}\"")),
            "the picker never writes {param} into the query"
        );
        assert!(
            chat.contains(&format!("query.get(\"{param}\")")),
            "the page never reads {param} back out of the query"
        );
    }

    // And what it read reaches the call. `session.create` is the only place
    // the contract takes them, so a page that read the query and did not put
    // them there would be exactly the defect this case exists for.
    let opening = chat
        .split_once("function opening()")
        .expect("chat.js composes session.create params in one function")
        .1;
    let body = &opening[..opening.find("\n}").expect("the function closes")];
    for param in ["provider", "model"] {
        assert!(
            body.contains(&format!("params.{param}")),
            "{param} never reaches session.create: {body}"
        );
    }
    assert!(
        chat.contains("call(\"session.create\", opening())"),
        "session.create is sent something other than those params"
    );

    // The button is still what sets them, so the two ends stay one feature.
    assert!(
        catalogue.contains("onStart(provider.provider, model)"),
        "the catalogue no longer hands a route and a model to its caller"
    );
}

/// TC-WEB-15: the catalogue marks the current conversation by route and model.
///
/// A model id is unique to a provider and not to a deployment: an official
/// route and a gateway both offering `gpt-5` is the ordinary case once a
/// document can declare providers, and marking by model alone puts "this
/// conversation" on both of them.
#[test]
fn the_catalogue_marks_the_current_entry_by_provider_and_model() {
    let catalogue = read("catalogue.js");
    let chat = read("chat.js");

    assert!(
        catalogue.contains("currentProvider"),
        "the catalogue still marks by model alone"
    );
    assert!(
        catalogue.contains("currentProvider === provider.provider"),
        "the mark does not compare the route"
    );
    assert!(
        chat.contains("currentProvider: runningProvider"),
        "the page never tells the catalogue which route it is on"
    );
    assert!(
        chat.contains("runningProvider = info.provider"),
        "the route is not taken from the session the server opened"
    );
}

/// TC-WEB-12: every script the page ships actually parses as a module.
///
/// This file says out loud that its readers are scans and not parsers, and
/// that is the right trade for the six structural claims above. It is the
/// wrong trade for one thing: a module that does not **parse** never runs at
/// all, so every other case here passes while the panel is a blank page. It
/// shipped exactly that way once - a new `const asked` beside the existing
/// `function asked` is `SyntaxError: Identifier 'asked' has already been
/// declared`, which stops `chat.js` before its first import, so no module is
/// fetched, no socket is dialled, and the page sits on the placeholders in
/// `index.html` with an empty console. Every scan in this file read that page
/// as correct.
///
/// The parser is the one already on the machine. `node --check` on a copy
/// named `.mjs` parses a module without resolving a single import, so this
/// needs no `node_modules`, no network and no dependency in this workspace -
/// which is what keeps it inside the project's "no Node runtime to install"
/// rule: a Node here is used if it happens to exist and is never required.
///
/// Where there is no Node, the case says so on stderr and falls back to
/// [`duplicated_top_level`], which catches the specific fault above without a
/// parser. It never passes on nothing.
#[test]
fn every_script_parses_as_a_module() {
    let Some(node) = node() else {
        eprintln!(
            "TC-WEB-12: no `node` on PATH, so the scripts were NOT parsed. \
             Falling back to the duplicate-declaration check, which is \
             narrower: install Node to get the whole claim."
        );
        assert_eq!(duplicated_top_level(), Vec::<String>::new());
        return;
    };

    let staged = tempfile::tempdir().expect("a temp dir");
    let mut broken = Vec::new();
    for (name, body) in scripts() {
        // Renamed on the way in: the extension is what tells the parser this
        // is a module, and a `.js` would be read as a script, where `import`
        // is a syntax error in every file the page ships.
        let path = staged
            .path()
            .join(format!("{}.mjs", name.trim_end_matches(".js")));
        std::fs::write(&path, &body).expect("a staged copy");
        let checked = std::process::Command::new(&node)
            .arg("--check")
            .arg(&path)
            .output()
            .expect("node runs");
        if !checked.status.success() {
            let said = String::from_utf8_lossy(&checked.stderr);
            // The first line naming the fault, not the whole stack, which is
            // about the temporary path and not about the page.
            let why = said
                .lines()
                .find(|line| line.contains("Error"))
                .unwrap_or("did not parse");
            broken.push(format!("{name}: {why}"));
        }
    }
    assert!(
        broken.is_empty(),
        "these scripts do not parse, so the page they are on never runs: {broken:?}"
    );
}

/// TC-WEB-13: no module declares one top-level name twice.
///
/// The narrower half of TC-WEB-12, kept as a case of its own because it is the
/// half that runs everywhere and because it names the fault rather than
/// reporting a parser's line number. `const asked` beside `function asked` is
/// the shape; so is a second `let` of a name a `const` already holds.
#[test]
fn no_module_declares_one_top_level_name_twice() {
    let clashes = duplicated_top_level();
    assert!(
        clashes.is_empty(),
        "these names are declared twice at the top level of one module, \
         which is a SyntaxError and stops the whole module: {clashes:?}"
    );
}

/// `node`, if this machine has one.
fn node() -> Option<String> {
    let found = std::process::Command::new("node").arg("--version").output();
    matches!(&found, Ok(out) if out.status.success()).then(|| "node".to_string())
}

/// Every top-level name each script declares more than once.
///
/// Top level is column zero: everything nested in this codebase is indented,
/// and a declaration that is not indented is not nested. That is a scan again,
/// but of a kind that cannot be fooled into silence the way the others were -
/// it looks for the collision itself rather than for a property that happens
/// to survive one.
fn duplicated_top_level() -> Vec<String> {
    let mut found = Vec::new();
    for (file, body) in scripts() {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for line in body.lines() {
            if line.starts_with(char::is_whitespace) {
                continue;
            }
            let Some(name) = declared_name(line) else {
                continue;
            };
            if !seen.insert(name.clone()) {
                found.push(format!("{file}: {name}"));
            }
        }
    }
    found.sort();
    found
}

/// The name a top-level declaration binds, if the line is one.
fn declared_name(line: &str) -> Option<String> {
    // `export` does not change what is bound, only who else can see it.
    let rest = line.strip_prefix("export ").unwrap_or(line);
    let rest = rest.strip_prefix("default ").unwrap_or(rest);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let (_, after) = ["const ", "let ", "var ", "function ", "class "]
        .into_iter()
        .find_map(|keyword| rest.strip_prefix(keyword).map(|after| (keyword, after)))?;
    let name: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    // A destructuring binding - `const { a, b } = ...` - starts with a brace
    // and binds several names; it is not this scan's business and answering
    // "" for it would collide with every other one.
    (!name.is_empty()).then_some(name)
}

/// Every string literal that follows `marker`, in either quote style.
///
/// Deliberately a scan rather than a parser. What it can miss is a computed
/// value - `getElementById(name)` - and missing one is a case that says nothing
/// rather than a case that lies; what a parser would buy is not worth a
/// dependency for six assertions.
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

/// The durable event types the engine writes.
///
/// Two signals, because the codebase uses two conventions and a rule that saw
/// only one would report a gap that is really a blind spot. A constant inside
/// a `mod topic` block is the declared form; a string literal handed straight
/// to `append` is the other. Method names are excluded by construction - those
/// live in `mod method`, and a scan that swept them in would demand the page
/// draw `tools/list`.
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
                // A test's own fixtures are not the engine's vocabulary.
                if path.file_name().is_some_and(|name| name == "tests") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                collect_topics(&src, &mut found);
            }
        }
    }
    assert!(
        found.len() > 20,
        "the topic scan stopped finding anything: {found:?}"
    );
    found
}

fn collect_topics(src: &str, found: &mut BTreeSet<String>) {
    for (at, _) in src.match_indices("mod topic") {
        let Some(open) = src[at..].find('{').map(|i| at + i + 1) else {
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
        let rest = &src[at..];
        let head: String = rest.chars().take(60).collect();
        if let Some(topic) = topic_literal(&head) {
            found.insert(topic);
        }
    }
}

/// The first `"family/name"` in a line, and nothing that is not one.
fn topic_literal(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = start + line[start..].find('"')?;
    let text = &line[start..end];
    let (family, name) = text.split_once('/')?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit())
    };
    (ok(family) && ok(name)).then(|| text.to_string())
}

/// Durable types the page draws through the raw fallback rather than a view.
///
/// Being here is a decision, not a defect: §4.3.2 says a surface passes an
/// unknown type through, and the raw rendering does exactly that. What it is
/// not is an accident, which is the point of writing them down.
const UNDRAWN: &[(&str, &str)] = &[
    (
        "workflow/start",
        "the workflow family landed after the last pass; four types that want \
         one view between them, showing a run's steps as they settle",
    ),
    ("workflow/step-start", "as above"),
    ("workflow/step-end", "as above"),
    ("workflow/end", "as above"),
    (
        "permission/preset",
        "log-only intent - the preset a person chose - and worth a line on the \
         transcript once the approval audit has somewhere to put it",
    ),
    (
        "fs/mode",
        "the filesystem knob, whose last value is the session's, exactly as \
         `approval/policy` works; it belongs beside that pill",
    ),
];

/// TC-WEB-10: every durable type the engine writes is drawn or listed.
///
/// The diff that found `question/asked` sitting undrawn for weeks while
/// `questions.js`'s own header claimed it, and then the whole `compaction/*`
/// family. Both were found by hand; this is the same comparison run by the
/// suite. It cannot say a type is drawn *well* - only that the page has an
/// opinion about the whole vocabulary rather than an accidental subset.
#[test]
fn the_page_accounts_for_every_durable_type_the_engine_writes() {
    let page: String = scripts().into_iter().map(|(_, body)| body).collect();
    let named: BTreeSet<String> = page
        .match_indices('"')
        .filter_map(|(at, _)| topic_literal(&page[at..]))
        .collect();
    let listed: BTreeSet<String> = UNDRAWN.iter().map(|(t, _)| t.to_string()).collect();
    let topics = durable_topics();

    let unaccounted: Vec<&String> = topics
        .iter()
        .filter(|t| !named.contains(*t) && !listed.contains(*t))
        .collect();
    assert!(
        unaccounted.is_empty(),
        "the engine writes these and the page says nothing about them - draw \
         them, or add a line to UNDRAWN saying why not: {unaccounted:?}"
    );

    let stale: Vec<&String> = listed.iter().filter(|t| named.contains(*t)).collect();
    assert!(
        stale.is_empty(),
        "these are listed as undrawn and the page now names them; drop them \
         from UNDRAWN: {stale:?}"
    );

    let gone: Vec<&String> = listed.iter().filter(|t| !topics.contains(*t)).collect();
    assert!(
        gone.is_empty(),
        "these are listed as undrawn and nothing writes them any more; drop \
         them from UNDRAWN: {gone:?}"
    );
}
