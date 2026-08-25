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
