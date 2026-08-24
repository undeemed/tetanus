//! Test Design Specification: skill discovery and invocation, ported.
//!
//! Feature under test: `tetanus_features::skill` - which roots are searched and
//! in what order, what a skill file may say, which candidates are reported as
//! faults, and what the model may load. Upstream pins the same decisions in
//! `packages/skill/skill-filesystem/tests/skill-filesystem.spec.ts` and
//! `packages/skill/skill/tests/skill.spec.ts`.
//!
//! Approach: real directories with real files on disk. Discovery is a question
//! about the filesystem - what is a directory, what a symlink points at, what a
//! read is refused - and a double answers a different question.
//!
//! What is not restated, and why. Upstream's provider registry (skills may come
//! from somewhere other than a filesystem) has one implementation here and
//! would be indirection with no second caller. Its root watcher, its durable
//! catalogue injection with the per-step tombstone protocol, its bundled
//! `skill-badge` asset, and its `resourceBase` hint for skills that ship files
//! alongside are named in `docs/parity.md`. Upstream reads its skill
//! files through `ctx.fs`; this reads them directly, because the fs service is
//! a consumer-facing seam for model-supplied paths and these paths are the
//! deployment's own.
//!
//! Environmental needs: a writable temporary directory. The symlink case is
//! Unix-only and compiles out elsewhere.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;
use tetanus_features::skill::{discover, read_skill, Root, SkillTool, Source};
use tetanus_turn::tools::Tool;

struct Tree {
    _dir: TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical");
        Self { _dir: dir, root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Write a file, creating whatever directories it needs.
    fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.path(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(&path, content).expect("write");
        path
    }

    /// A skill in the usual shape: frontmatter, then instructions.
    fn skill(&self, relative: &str, description: &str, body: &str) -> PathBuf {
        self.write(
            relative,
            &format!("---\ndescription: {description}\n---\n\n{body}\n"),
        )
    }
}

fn roots(project: &Path) -> Vec<Root> {
    vec![
        Root::new(project.join(".tetanus/skills"), Source::Project),
        Root::new(project.join(".agents/skills"), Source::ProjectAgents),
        Root::new(project.join("home/skills"), Source::User),
    ]
}

/// TC-PORT-SKILL-1: both file shapes are discovered.
///
/// Upstream: "parses flat skills", and directory bundles are `<name>/SKILL.md`.
///
/// Input: a directory bundle and a flat Markdown file in one root.
/// Expected: both found, named for their directory or file stem. Both spellings
/// exist in the wild, and reading only one would silently ignore half of what a
/// user wrote.
#[test]
fn a_bundle_and_a_flat_file_are_both_skills() {
    let tree = Tree::new();
    tree.skill(
        ".tetanus/skills/deploy/SKILL.md",
        "how to deploy this service",
        "Run the pipeline.",
    );
    tree.skill(
        ".tetanus/skills/release.md",
        "how to cut a release",
        "Tag it.",
    );

    let roster = discover(&roots(&tree.root));

    assert_eq!(
        roster.skills.keys().collect::<Vec<_>>(),
        ["deploy", "release"]
    );
    assert_eq!(roster.skills["deploy"].content, "Run the pipeline.");
    assert!(roster.faults.is_empty(), "{:?}", roster.faults);
}

/// TC-PORT-SKILL-2: roots are searched in order and the earlier one wins.
///
/// Upstream: "discovers project, custom, user, and agents skill roots in
/// priority order", and "lets project skills override ... user skills".
///
/// Input: the same name defined in all three roots.
/// Expected: the project's wins; the other two are recorded as shadowed, naming
/// what displaced them. Dropping them silently is what leaves "my skill does
/// nothing" with no answer anywhere - the rule `crates/config/src/preset.rs`
/// already settled for presets.
#[test]
fn the_earlier_root_wins_and_the_loser_is_recorded() {
    let tree = Tree::new();
    let winner = tree.skill(".tetanus/skills/deploy.md", "the project's", "Project.");
    tree.skill(".agents/skills/deploy.md", "the shared one", "Agents.");
    tree.skill("home/skills/deploy.md", "the user's", "User.");

    let roster = discover(&roots(&tree.root));

    assert_eq!(roster.skills.len(), 1);
    assert_eq!(roster.skills["deploy"].content, "Project.");
    assert_eq!(roster.skills["deploy"].source, Source::Project);
    assert_eq!(roster.shadowed.len(), 2);
    assert_eq!(roster.shadowed[0].source, Source::ProjectAgents);
    assert_eq!(roster.shadowed[0].by, winner);
    assert_eq!(roster.shadowed[1].source, Source::User);
}

/// TC-PORT-SKILL-3: a root that is not there is not a fault; one that cannot be
/// read is.
///
/// Upstream: "reports transient root reads as incomplete without caching an
/// empty catalog".
///
/// Input: a missing root, and one whose permissions deny a read.
/// Expected: nothing said about the missing one - most deployments have two of
/// the five roots - and a fault naming the unreadable one. Answering "no skills
/// here" for a directory this process was denied would serve a roster nobody
/// chose.
#[cfg(unix)]
#[test]
fn a_missing_root_is_ordinary_and_an_unreadable_one_is_a_fault() {
    use std::os::unix::fs::PermissionsExt;

    let tree = Tree::new();
    tree.skill(".tetanus/skills/deploy.md", "the project's", "Project.");
    let denied = tree.path("home/skills");
    std::fs::create_dir_all(&denied).expect("dirs");
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    let roster = discover(&roots(&tree.root));

    // Restored before any assertion can fail, so a failing case still leaves a
    // removable temporary directory behind.
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    assert_eq!(
        roster.skills.len(),
        1,
        "the readable root still contributed"
    );
    assert_eq!(roster.faults.len(), 1, "{:?}", roster.faults);
    assert_eq!(roster.faults[0].path, denied);
    assert!(
        roster.faults[0].reason.contains("could not be read"),
        "{}",
        roster.faults[0].reason
    );
}

/// TC-PORT-SKILL-4: a broken candidate is reported and never hides a valid
/// sibling.
///
/// Upstream: "filters invalid skills from the invocation-neutral listing",
/// "skips invalid YAML skill files without hiding valid siblings", and "rejects
/// legacy and invalid invocation frontmatter without hiding valid siblings".
///
/// Input: one good skill beside three broken ones - no description, an
/// unreadable boolean, and a legacy invocation key.
/// Expected: the good one is offered; each broken one is a fault naming its
/// path and its reason. A skill that simply fails to appear gives its author
/// nowhere to look.
#[test]
fn a_broken_candidate_is_a_fault_and_its_siblings_still_load() {
    let tree = Tree::new();
    tree.skill(".tetanus/skills/good.md", "a working skill", "Do it.");
    tree.write(
        ".tetanus/skills/nameless.md",
        "---\nname: x\n---\nno description",
    );
    tree.write(
        ".tetanus/skills/spelling.md",
        "---\ndescription: d\ndisable-model-invocation: maybe\n---\nbody",
    );
    tree.write(
        ".tetanus/skills/legacy.md",
        "---\ndescription: d\ndisableModelInvocation: true\n---\nbody",
    );

    let roster = discover(&roots(&tree.root));

    assert_eq!(roster.skills.keys().collect::<Vec<_>>(), ["good"]);
    assert_eq!(roster.faults.len(), 3, "{:?}", roster.faults);
    let reasons: String = roster
        .faults
        .iter()
        .map(|fault| format!("{}: {}\n", fault.path.display(), fault.reason))
        .collect();
    assert!(reasons.contains("no `description`"), "{reasons}");
    assert!(reasons.contains("must be true or false"), "{reasons}");
    assert!(
        reasons.contains("is not a key this build reads"),
        "{reasons}"
    );
}

/// TC-PORT-SKILL-5: the documented boolean spellings, and what they do.
///
/// Upstream: "accepts the documented boolean spellings for invocation
/// frontmatter".
///
/// Input: `true`, `yes`, `on`, `false`, `no`, `off`, in both cases and quoted.
/// Expected: the three affirmatives keep a skill out of the model's catalogue
/// and the three negatives leave it in. A closed list rather than "anything
/// truthy", because reading an unknown spelling as false fails open - it offers
/// the model a skill somebody meant to keep back.
#[test]
fn the_boolean_spellings_are_a_closed_list() {
    let tree = Tree::new();
    for (index, spelling) in ["true", "YES", "\"on\""].iter().enumerate() {
        tree.write(
            &format!(".tetanus/skills/held{index}.md"),
            &format!("---\ndescription: d\ndisable-model-invocation: {spelling}\n---\nbody"),
        );
    }
    for (index, spelling) in ["false", "No", "'off'"].iter().enumerate() {
        tree.write(
            &format!(".tetanus/skills/open{index}.md"),
            &format!("---\ndescription: d\ndisable-model-invocation: {spelling}\n---\nbody"),
        );
    }

    let roster = discover(&roots(&tree.root));

    assert!(roster.faults.is_empty(), "{:?}", roster.faults);
    assert_eq!(roster.skills.len(), 6);
    let offered: Vec<&str> = roster
        .model_invocable()
        .into_iter()
        .map(|skill| skill.name.as_str())
        .collect();
    assert_eq!(offered, ["open0", "open1", "open2"]);
}

/// TC-PORT-SKILL-6: CRLF frontmatter parses, and a fence inside a value does
/// not end the block.
///
/// Upstream: "supports CRLF frontmatter and ignores delimiter-looking text
/// inside YAML values".
///
/// Input: a skill written with Windows line endings whose description contains
/// three dashes.
/// Expected: it parses, with the whole description and the body intact. A file
/// written on Windows is a file, and a `---` inside a value is text.
#[test]
fn crlf_parses_and_a_fence_inside_a_value_is_text() {
    let tree = Tree::new();
    tree.write(
        ".tetanus/skills/windows.md",
        "---\r\ndescription: a rule --- with dashes\r\n---\r\n\r\nThe body.\r\n",
    );

    let roster = discover(&roots(&tree.root));

    assert!(roster.faults.is_empty(), "{:?}", roster.faults);
    let skill = &roster.skills["windows"];
    assert_eq!(skill.description, "a rule --- with dashes");
    assert_eq!(skill.content, "The body.");
}

/// TC-PORT-SKILL-7: the frontmatter name wins over the file name.
///
/// Upstream: the parsed candidate's name is its identity.
///
/// Input: a file called `deploy.md` whose frontmatter names it `ship`.
/// Expected: it is `ship`, and nothing answers to `deploy`. The file name is a
/// fallback so a skill cannot be unnamed, not an override of what the author
/// wrote.
#[test]
fn the_name_in_the_file_wins_over_the_file_name() {
    let tree = Tree::new();
    tree.write(
        ".tetanus/skills/deploy.md",
        "---\nname: ship\ndescription: how to ship\n---\nBody.",
    );

    let roster = discover(&roots(&tree.root));

    assert_eq!(roster.skills.keys().collect::<Vec<_>>(), ["ship"]);
}

/// TC-PORT-SKILL-8: a symlinked skill is discovered.
///
/// Upstream: "discovers symlinked skill directories and flat files".
///
/// Input: a skill outside the root, linked into it.
/// Expected: found. A user who keeps their skills in a checkout and links them
/// into place has done a normal thing.
#[cfg(unix)]
#[test]
fn a_symlinked_skill_is_discovered() {
    let tree = Tree::new();
    let real = tree.skill("elsewhere/deploy.md", "the linked one", "Linked.");
    std::fs::create_dir_all(tree.path(".tetanus/skills")).expect("dirs");
    std::os::unix::fs::symlink(&real, tree.path(".tetanus/skills/deploy.md")).expect("symlink");

    let roster = discover(&roots(&tree.root));

    assert_eq!(roster.skills["deploy"].content, "Linked.");
}

/// TC-PORT-SKILL-9: the catalogue is what the model is offered, and only that.
///
/// Upstream: "returns an invocation-neutral catalog and resolves model and user
/// policy independently", and its sorted model-visible summaries.
///
/// Input: two model-invocable skills and one held back.
/// Expected: the tool's description lists the two in name order, the argument
/// enum holds exactly those two, and the roster still knows about all three - a
/// skill kept from the model is not a skill the harness has forgotten.
#[test]
fn the_catalogue_lists_what_the_model_may_ask_for() {
    let tree = Tree::new();
    tree.skill(".tetanus/skills/zeta.md", "the last one", "Z.");
    tree.skill(".tetanus/skills/alpha.md", "the first one", "A.");
    tree.write(
        ".tetanus/skills/private.md",
        "---\ndescription: run by a person\ndisable-model-invocation: true\n---\nP.",
    );
    let roster = Arc::new(discover(&roots(&tree.root)));

    let schema = SkillTool::new(Arc::clone(&roster)).schema();

    assert_eq!(roster.skills.len(), 3, "all three were discovered");
    assert_eq!(
        schema.parameters["properties"]["name"]["enum"],
        json!(["alpha", "zeta"])
    );
    assert!(
        schema.description.contains("- alpha: the first one"),
        "{}",
        schema.description
    );
    assert!(
        !schema.description.contains("private"),
        "{}",
        schema.description
    );
}

/// TC-PORT-SKILL-10: loading a skill hands over its instructions.
///
/// Upstream: `renderSkillContent` frames the skill for the model.
///
/// Input: the tool called with a known name.
/// Expected: `ok`, with the skill's name and its body. The frame names the
/// skill so a model reading a long transcript can tell instructions it loaded
/// from instructions it was born with.
#[tokio::test]
async fn loading_a_skill_hands_over_its_instructions() {
    let tree = Tree::new();
    tree.skill(
        ".tetanus/skills/deploy/SKILL.md",
        "how to deploy",
        "1. Run the pipeline.\n2. Watch the dashboard.",
    );
    let tool = SkillTool::new(Arc::new(discover(&roots(&tree.root))));

    let outcome = tool
        .execute(&json!({ "name": "deploy" }))
        .await
        .expect("ran");

    assert!(outcome.ok);
    assert!(
        outcome.content.starts_with("# Skill: deploy"),
        "{}",
        outcome.content
    );
    assert!(
        outcome.content.contains("Watch the dashboard."),
        "{}",
        outcome.content
    );
}

/// TC-PORT-SKILL-11: an unknown name, and a name the model may not use, are
/// both refused with what to do instead.
///
/// Upstream resolves model policy at invocation, not only in the listing.
///
/// Input: a name nobody defined, and the name of a skill held back from the
/// model.
/// Expected: both refused; the first lists what is available, the second says
/// the skill is a person's to run. Checking the policy only when building the
/// catalogue would let a model that guessed the name have it.
#[tokio::test]
async fn an_unknown_skill_and_a_held_back_one_are_both_refused() {
    let tree = Tree::new();
    tree.skill(".tetanus/skills/deploy.md", "how to deploy", "Body.");
    tree.write(
        ".tetanus/skills/private.md",
        "---\ndescription: run by a person\ndisable-model-invocation: true\n---\nP.",
    );
    let tool = SkillTool::new(Arc::new(discover(&roots(&tree.root))));

    let unknown = tool.execute(&json!({ "name": "nope" })).await.expect("ran");
    let held = tool
        .execute(&json!({ "name": "private" }))
        .await
        .expect("ran");

    assert!(!unknown.ok);
    assert!(
        unknown.content.contains("- deploy: how to deploy"),
        "{}",
        unknown.content
    );
    assert!(!held.ok);
    assert!(
        held.content.contains("run deliberately by a person"),
        "{}",
        held.content
    );
}

/// TC-PORT-SKILL-12: a file that is not frontmatter at all is still a skill's
/// content, and a file with no frontmatter is a fault for the right reason.
///
/// Upstream parses the file and validates the parsed candidate's fields.
///
/// Input: a Markdown file starting with a horizontal rule but no closing fence,
/// read directly.
/// Expected: refused for having no description rather than for a parse failure.
/// Treating an unterminated fence as a broken document would make a file that
/// merely starts with a rule unreadable, and the reason a reader is given
/// should be the one they can act on.
#[test]
fn an_unterminated_fence_is_content_and_the_fault_names_the_real_problem() {
    let tree = Tree::new();
    let path = tree.write(
        ".tetanus/skills/rule.md",
        "---\njust a rule, no fence\n\nBody.",
    );

    let refused = read_skill(&path, "rule", Source::Project).expect_err("refused");

    assert!(refused.contains("no `description`"), "{refused}");
}
