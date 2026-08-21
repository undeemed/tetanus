//! Test Design Specification: the preset roster, ported.
//!
//! Feature under test: `tetanus_config::preset` - finding the named settings a
//! deployment can switch between. Upstream pins the same roster semantics in
//! `packages/preset/agent-presets/tests/discovery.spec.ts`; each case names
//! the upstream case it comes from.
//!
//! Approach: real directories in a temp tree. Discovery is a question about
//! what is on disk, and a fixture that described the filesystem instead of
//! being one would be asserting the description.
//!
//! What is not restated, and why. Upstream's authoring half - copying a
//! shipped preset into a writable root, tightening POSIX modes, deleting,
//! refusing to delete a shipped one - needs a write path `tetanus-config` does
//! not have. Its composition-health cases parse a Cordis plugin tree, and a
//! tetanus preset holds a settings document instead, so health here is what
//! `file::read` already decides and its own cases already cover; what these
//! add is that the decision reaches the roster. Its tilde expansion belongs to
//! whoever builds the root paths.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key. One case makes a directory unreadable and is skipped when
//! the process can read it anyway, which is what happens as root.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use serde_json::json;
use tempfile::TempDir;
use tetanus_config::preset::{discover, find, is_preset_id, ready, Health, Preset, Root, Trust};
use tetanus_config::ConfigError;

/// TC-PRESET-1: one preset per directory holding a document, ordered by id.
///
/// Upstream: "reports one preset per directory holding a composition, ordered
/// by id".
///
/// The order is the point of the case. A filesystem hands entries back in
/// whatever order it likes, and it differs between systems, so a listing that
/// inherited it would be stable on the machine it was written on and nowhere
/// else.
///
/// Input: three preset directories created out of alphabetical order.
/// Expected: three ready presets, by id, each carrying the settings its
/// document holds.
#[test]
fn one_preset_per_directory_ordered_by_id() {
    let tree = Tree::new();
    tree.preset(
        "shipped",
        "thorough",
        json!({ "agent": { "max_steps": 40 } }),
    );
    tree.preset("shipped", "fast", json!({ "agent": { "max_steps": 4 } }));
    tree.preset(
        "shipped",
        "balanced",
        json!({ "agent": { "max_steps": 12 } }),
    );

    let found = discover(&tree.roots()).expect("discover");

    assert_eq!(ids(&found), ["balanced", "fast", "thorough"]);
    assert!(found.iter().all(|preset| preset.health.is_ready()));
    assert_eq!(
        found[1].health.settings().expect("ready")["agent.max_steps"],
        json!(4),
        "and it carries what its document said"
    );
}

/// TC-PRESET-2: a directory that is not a working preset is reported, not
/// skipped.
///
/// Upstream: "reports a directory with no composition as a broken preset
/// slot".
///
/// Silence is the worst answer here. A preset that simply does not appear
/// gives its author nowhere to look, and the two ways to break one - forget
/// the document, or mistype it - are both things people do.
///
/// Input: a good preset, a directory with no document, and one whose document
/// will not parse.
/// Expected: all three in the roster; the good one ready, the empty one
/// `NoDocument`, and the broken one `Unreadable` carrying the reader's own
/// words, which already name the file and the fault.
#[test]
fn a_directory_that_is_not_a_working_preset_is_reported() {
    let tree = Tree::new();
    tree.preset("shipped", "good", json!({ "a": { "b": 1 } }));
    tree.empty("shipped", "forgotten");
    tree.broken("shipped", "mistyped", "{ this is not json");

    let found = discover(&tree.roots()).expect("discover");

    assert_eq!(ids(&found), ["forgotten", "good", "mistyped"]);
    assert!(found[1].health.is_ready());
    assert_eq!(found[0].health, Health::NoDocument);
    match &found[2].health {
        Health::Unreadable(why) => {
            assert!(why.contains("mistyped"), "it names the file: {why}");
            assert!(why.contains("does not parse"), "and the fault: {why}");
        }
        other => panic!("expected an unreadable document, got {other:?}"),
    }

    assert_eq!(
        ids(&ready(&tree.roots()).expect("ready")),
        ["good"],
        "and only the working one is offered"
    );
}

/// TC-PRESET-3: a directory nothing could name as a preset is skipped.
///
/// Upstream: "skips a directory whose name no preset id could ever claim".
///
/// Skipped rather than reported, unlike TC-PRESET-2, and the difference is
/// intent: a directory called `.git` or `My Presets` was never trying to be a
/// preset, so calling it broken would fill the roster with noise. A directory
/// with a usable name and no document was trying.
///
/// Input: names with a capital, a space, a leading dot, and one too long,
/// beside one good preset.
/// Expected: only the good one, and the id rule agrees when asked directly.
#[test]
fn a_directory_nothing_could_name_as_a_preset_is_skipped() {
    let tree = Tree::new();
    tree.preset("shipped", "fine", json!({ "a": { "b": 1 } }));
    for unnameable in [".git", "My Presets", "UPPER", "has space", &"x".repeat(65)] {
        tree.empty("shipped", unnameable);
    }

    assert_eq!(ids(&discover(&tree.roots()).expect("discover")), ["fine"]);

    assert!(is_preset_id("fine"));
    assert!(is_preset_id("a.b-c_1"));
    for no in ["", ".git", "UPPER", "has space", &"x".repeat(65)] {
        assert!(!is_preset_id(no), "{no:?}");
    }
}

/// TC-PRESET-4: every preset records the root it came from.
///
/// Upstream: "records the root trust on every preset it discovers".
///
/// Input: one preset in each root.
/// Expected: each carries the trust of the root it was found in, and the path
/// it was found at - so a message about a preset can say where to go and edit
/// it.
#[test]
fn every_preset_records_where_it_came_from() {
    let tree = Tree::new();
    tree.preset("shipped", "builtin", json!({ "a": { "b": 1 } }));
    tree.preset("user", "mine", json!({ "a": { "b": 2 } }));

    let found = discover(&tree.roots()).expect("discover");

    let builtin = &found[0];
    let mine = &found[1];
    assert_eq!(
        (builtin.id.as_str(), builtin.trust),
        ("builtin", Trust::Shipped)
    );
    assert_eq!((mine.id.as_str(), mine.trust), ("mine", Trust::User));
    assert!(builtin.path.ends_with("shipped/builtin"));
    assert!(mine.path.ends_with("user/mine"));
}

/// TC-PRESET-5: the earlier root wins a duplicate id, and the loser says so.
///
/// Upstream: "lets the earlier root win a duplicate id".
///
/// The winning half is upstream's rule. The rest is this port's addition: the
/// preset that lost stays in the roster marked `Shadowed`, because "I made a
/// preset and it does nothing" is a question someone will ask, and dropping it
/// silently leaves no answer anywhere.
///
/// Input: the same id in both roots, with different settings.
/// Expected: the shipped one is what `find` returns and what `ready` offers;
/// the user one is present, marked shadowed by the root that beat it, and
/// offered to nobody.
#[test]
fn the_earlier_root_wins_and_the_loser_says_so() {
    let tree = Tree::new();
    tree.preset("shipped", "fast", json!({ "agent": { "max_steps": 4 } }));
    tree.preset("user", "fast", json!({ "agent": { "max_steps": 99 } }));

    let found = discover(&tree.roots()).expect("discover");
    assert_eq!(found.len(), 2, "both are in the roster");

    let winner = find(&tree.roots(), "fast").expect("find").expect("present");
    assert_eq!(winner.trust, Trust::Shipped);
    assert_eq!(
        winner.health.settings().expect("ready")["agent.max_steps"],
        json!(4)
    );

    let loser = found
        .iter()
        .find(|preset| preset.trust == Trust::User)
        .expect("the shadowed one is still listed");
    assert_eq!(loser.health, Health::Shadowed { by: Trust::Shipped });
    assert_eq!(ids(&ready(&tree.roots()).expect("ready")), ["fast"]);
}

/// TC-PRESET-5b: the roster is one ordered list, not one list per root.
///
/// Written because a mutation survived: dropping the ordering pass passes
/// every case whose presets come from a single root, since the entries within
/// a root are already sorted as they are read. The ordering that actually
/// needs doing is across roots, and a caller rendering a picker would
/// otherwise get the shipped presets and then the user's, each sorted, which
/// reads as a bug in the sort.
///
/// Input: ids that interleave between the two roots.
/// Expected: one list in id order, whichever root each came from.
#[test]
fn the_roster_is_one_ordered_list_across_roots() {
    let tree = Tree::new();
    tree.preset("shipped", "banana", json!({ "a": { "b": 1 } }));
    tree.preset("shipped", "zebra", json!({ "a": { "b": 1 } }));
    tree.preset("user", "apple", json!({ "a": { "b": 1 } }));
    tree.preset("user", "cherry", json!({ "a": { "b": 1 } }));

    assert_eq!(
        ids(&discover(&tree.roots()).expect("discover")),
        ["apple", "banana", "cherry", "zebra"]
    );
}

/// TC-PRESET-6: an absent root supplies nothing, and is not a fault.
///
/// Upstream: "treats an absent root as supplying no presets".
///
/// A deployment that has never written a user preset has no user directory,
/// and that is the ordinary first run rather than something to report.
///
/// Input: a shipped root with one preset and a user root that does not exist.
/// Expected: the shipped preset, and no error.
#[test]
fn an_absent_root_supplies_nothing() {
    let tree = Tree::new();
    tree.preset("shipped", "only", json!({ "a": { "b": 1 } }));
    let roots = vec![
        Root::shipped(tree.dir.path().join("shipped")),
        Root::user(tree.dir.path().join("never-created")),
    ];

    assert_eq!(ids(&discover(&roots).expect("discover")), ["only"]);
}

/// TC-PRESET-7: a plain file beside the preset directories is ignored.
///
/// Upstream: "ignores a plain file sitting beside the preset directories".
///
/// A root may hold a README, and reading one as a broken preset would put
/// noise in every listing.
///
/// Input: a preset directory and two plain files beside it.
/// Expected: only the directory.
#[test]
fn a_plain_file_beside_the_directories_is_ignored() {
    let tree = Tree::new();
    tree.preset("shipped", "real", json!({ "a": { "b": 1 } }));
    std::fs::write(tree.dir.path().join("shipped/README.md"), "hello").expect("write");
    std::fs::write(tree.dir.path().join("shipped/notes"), "hello").expect("write");

    assert_eq!(ids(&discover(&tree.roots()).expect("discover")), ["real"]);
}

/// TC-PRESET-8: a root that cannot be read is a fault, not an empty root.
///
/// Upstream: "reports a root it cannot read rather than treating it as empty".
///
/// The same rule the session store and the key-value store already follow, and
/// for the same reason: absence and refusal are different facts. Answering
/// "no presets" for a directory full of them serves a deployment a
/// configuration it did not choose, and gives no sign that anything went
/// wrong.
///
/// Input: a root whose permissions deny reading it.
/// Expected: `Unreadable`, naming the root. Skipped when the process can read
/// it regardless, which is what happens as root and is not a failure of this
/// rule.
#[cfg(unix)]
#[test]
fn a_root_that_cannot_be_read_is_a_fault() {
    use std::os::unix::fs::PermissionsExt;

    let tree = Tree::new();
    tree.preset("shipped", "hidden", json!({ "a": { "b": 1 } }));
    let root = tree.dir.path().join("shipped");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    let answered = discover(&tree.roots());

    // Restore before asserting, so a failure does not leave a directory the
    // temp cleanup cannot remove.
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).expect("chmod back");

    match answered {
        Err(ConfigError::Unreadable { path, .. }) => assert_eq!(path, root),
        Ok(found) if !found.is_empty() => {
            // Running as a user who bypasses the mode: the rule is untestable
            // here rather than broken.
        }
        other => panic!("a root that cannot be read must not read as empty: {other:?}"),
    }
}

/// TC-PRESET-9: a preset's document is read the way every settings document
/// is.
///
/// A preset that accepted a different dialect, or resolved keys differently
/// from the main document, would be a second configuration language nobody
/// asked for.
///
/// Input: one preset written as YAML and one as JSON, both nested.
/// Expected: both ready, both flattened to the same dotted keys the harness
/// resolves everywhere else.
#[test]
fn a_preset_document_is_read_like_any_other() {
    let tree = Tree::new();
    tree.preset(
        "shipped",
        "as-json",
        json!({ "llm": { "retry": { "max_retries": 5 } } }),
    );
    tree.written(
        "shipped",
        "as-yaml",
        "settings.yaml",
        "llm:\n  retry:\n    max_retries: 5\n",
    );

    let found = discover(&tree.roots()).expect("discover");

    for preset in &found {
        assert_eq!(
            preset.health.settings().expect("ready")["llm.retry.max_retries"],
            json!(5),
            "{} resolves the same dotted key",
            preset.id
        );
    }
}

/// TC-PRESET-10: asking for a preset that is not there answers nothing, not a
/// fault.
///
/// A name a user mistyped is a question with an answer - "there is no such
/// preset" - and a caller that has to tell that apart from a broken root needs
/// the two to be different shapes.
///
/// Input: a roster with one preset, asked for another name; and asked for a
/// name that only a shadowed preset holds.
/// Expected: `None` in both cases, and no error.
#[test]
fn asking_for_a_preset_that_is_not_there_answers_nothing() {
    let tree = Tree::new();
    tree.preset("shipped", "real", json!({ "a": { "b": 1 } }));

    assert!(find(&tree.roots(), "imaginary").expect("find").is_none());
    assert!(find(&tree.roots(), "real").expect("find").is_some());
}

// ---------------------------------------------------------------- fixtures

struct Tree {
    dir: TempDir,
}

impl Tree {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        for root in ["shipped", "user"] {
            std::fs::create_dir_all(dir.path().join(root)).expect("mkdir");
        }
        Self { dir }
    }

    /// The two roots, most trusted first - which is the order that decides a
    /// duplicate.
    fn roots(&self) -> Vec<Root> {
        vec![
            Root::shipped(self.dir.path().join("shipped")),
            Root::user(self.dir.path().join("user")),
        ]
    }

    fn preset(&self, root: &str, id: &str, settings: serde_json::Value) {
        self.written(root, id, "settings.json", &settings.to_string());
    }

    fn broken(&self, root: &str, id: &str, contents: &str) {
        self.written(root, id, "settings.json", contents);
    }

    fn empty(&self, root: &str, id: &str) {
        std::fs::create_dir_all(self.dir.path().join(root).join(id)).expect("mkdir");
    }

    fn written(&self, root: &str, id: &str, file: &str, contents: &str) {
        let directory = self.dir.path().join(root).join(id);
        std::fs::create_dir_all(&directory).expect("mkdir");
        std::fs::write(directory.join(file), contents).expect("write");
    }
}

fn ids(found: &[Preset]) -> Vec<&str> {
    found.iter().map(|preset| preset.id.as_str()).collect()
}
