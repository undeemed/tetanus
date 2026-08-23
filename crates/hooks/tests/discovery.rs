//! Conformance: finding a deployment's hook configuration on disk.
//!
//! Feature under test: `tetanus_hooks::discovery` — reading a configuration
//! file, telling its three failure modes apart, and handing a bridge the
//! groups and the environment a hook process needs.
//!
//! Ported from the `configPath` half of upstream
//! `packages/hooks/hooks-claude-code/src/index.ts` and
//! `packages/hooks/hooks-codex/src/index.ts`, whose `apply` reads one path at
//! load. Case ids TC-HOOK-DISC-1..11.
//!
//! What is not restated, and why. Upstream validates its plugin config through
//! schemastery at load; tetanus has no plugin config service, so what is
//! asserted here is the reading and the parsing, and the caller supplies the
//! path. Upstream's `TODO(per-session-hook-config)` - discovering a
//! project-local file from each session's cwd - is not built there either, and
//! `conventional_path` spells the candidate without going looking for it.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value.

use std::path::Path;

use tetanus_hooks::discovery::{conventional_path, Discovery, LoadError, CLAUDE_PROJECT_DIR};
use tetanus_hooks::events::HookDialect;

fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write the config");
    path
}

const CLAUDE_HOOKS: &str = r#"{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Bash", "hooks": [{ "type": "command", "command": "guard.sh" }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "done.sh" }] }
    ]
  }
}"#;

/// TC-HOOK-DISC-1: a configured file becomes runnable groups, per point.
#[test]
fn a_configured_file_becomes_groups_per_point() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(dir.path(), "settings.json", CLAUDE_HOOKS);

    let found = Discovery::at(&path)
        .load(HookDialect::ClaudeCode)
        .expect("a readable config");

    assert_eq!(found.event("PreToolUse").len(), 1);
    assert_eq!(found.event("Stop").len(), 1);
    assert!(found.event("PostToolUse").is_empty(), "none configured");
    assert!(!found.is_empty());
}

/// TC-HOOK-DISC-2: a file that is not there is not an error.
///
/// Most deployments configure no hooks. Making absence an error would force
/// every composition site to ask first, and the check that gets forgotten is
/// the one that turns "no hooks" into a startup failure.
#[test]
fn an_absent_file_is_an_empty_configuration() {
    let dir = tempfile::tempdir().expect("temp dir");
    let found = Discovery::at(dir.path().join("nothing-here.json"))
        .load(HookDialect::ClaudeCode)
        .expect("an absent file is not a failure");

    assert!(found.is_empty());
    assert!(found.event("PreToolUse").is_empty());
}

/// TC-HOOK-DISC-3: a file that exists and will not parse is reported, and the
/// three failures are told apart.
///
/// Somebody wrote that file on purpose. Reading a typo as "no hooks
/// configured" would silently drop every guard a deployment believed it had,
/// which is the worst outcome available here - the deployment thinks it is
/// protected and is not.
#[test]
fn a_file_that_will_not_parse_is_reported_and_says_which_way() {
    let dir = tempfile::tempdir().expect("temp dir");

    let bad_json = write(dir.path(), "broken.json", "{ this is not json");
    match Discovery::at(&bad_json).load(HookDialect::ClaudeCode) {
        Err(LoadError::NotJson { path, reason }) => {
            assert!(path.ends_with("broken.json"), "names the file: {path}");
            assert!(!reason.is_empty(), "and says what was wrong");
        }
        other => panic!("expected NotJson, got {other:?}"),
    }

    // A directory is readable-as-a-path but not as a file, which is the
    // ordinary way a misconfigured path fails.
    match Discovery::at(dir.path()).load(HookDialect::ClaudeCode) {
        Err(LoadError::Unreadable { path, .. }) => {
            assert!(!path.is_empty());
        }
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

/// TC-HOOK-DISC-4: every failure names the file.
///
/// A deployment may configure one file per dialect. A message that does not
/// say which one sends somebody to edit the wrong file.
#[test]
fn every_failure_names_the_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(dir.path(), "codex-hooks.json", "not json at all");
    let error = Discovery::at(&path)
        .load(HookDialect::Codex)
        .expect_err("a parse failure");
    assert!(error.to_string().contains("codex-hooks.json"), "{error}");
}

/// TC-HOOK-DISC-5: `${CLAUDE_PROJECT_DIR}` is substituted into commands.
#[test]
fn the_project_dir_is_substituted_into_commands() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(
        dir.path(),
        "settings.json",
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"${CLAUDE_PROJECT_DIR}/bin/guard.sh"}]}]}}"#,
    );

    let found = Discovery::at(&path)
        .in_project("/srv/app")
        .load(HookDialect::ClaudeCode)
        .expect("a readable config");

    assert_eq!(
        found.event("PreToolUse")[0].hooks[0].command,
        "/srv/app/bin/guard.sh"
    );
}

/// TC-HOOK-DISC-6: an unset token is left alone rather than blanked.
///
/// `${CLAUDE_PLUGIN_ROOT}/x.sh` becoming `/x.sh` would name a real and quite
/// different path - one that might exist and might be somebody else's script.
#[test]
fn an_unset_token_is_left_verbatim() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(
        dir.path(),
        "settings.json",
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/x.sh"}]}]}}"#,
    );

    let found = Discovery::at(&path)
        .load(HookDialect::ClaudeCode)
        .expect("a readable config");

    assert_eq!(
        found.event("PreToolUse")[0].hooks[0].command,
        "${CLAUDE_PLUGIN_ROOT}/x.sh",
        "left as written, not resolved against the filesystem root"
    );
}

/// TC-HOOK-DISC-7: the project directory is exported to the hook process.
///
/// Claude Code always exports it, and unmodified hooks use it to find
/// project-relative files. A bridge that dropped it would run those hooks
/// successfully and have them look in the wrong place, which is worse than not
/// running them: the hook reports success about the wrong directory.
#[test]
fn the_project_dir_is_exported_to_the_hook_process() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(dir.path(), "settings.json", CLAUDE_HOOKS);

    let found = Discovery::at(&path)
        .in_project("/srv/app")
        .load(HookDialect::ClaudeCode)
        .expect("a readable config");
    assert_eq!(
        found.env(),
        [(CLAUDE_PROJECT_DIR.to_owned(), "/srv/app".to_owned())]
    );

    // And when nobody named one, nothing is exported rather than an empty
    // string: a hook reading `$CLAUDE_PROJECT_DIR` should find it unset, not
    // find it set to the filesystem root.
    let unset = Discovery::at(&path)
        .load(HookDialect::ClaudeCode)
        .expect("a readable config");
    assert!(unset.env().is_empty());
}

/// TC-HOOK-DISC-8: the substituted value and the exported value are the same.
///
/// They come from one field for exactly this reason. If a command could be
/// rewritten against one directory while the process was told about another,
/// a hook would be launched pointing at one tree and reading from a second.
#[test]
fn the_substituted_and_exported_project_dir_cannot_disagree() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(
        dir.path(),
        "settings.json",
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"${CLAUDE_PROJECT_DIR}/s.sh"}]}]}}"#,
    );

    let found = Discovery::at(&path)
        .in_project("/w")
        .load(HookDialect::ClaudeCode)
        .expect("a readable config");

    let exported = found
        .env()
        .into_iter()
        .find(|(k, _)| k == CLAUDE_PROJECT_DIR)
        .map(|(_, v)| v)
        .expect("exported");
    assert!(found.event("Stop")[0].hooks[0]
        .command
        .starts_with(&exported));
}

/// TC-HOOK-DISC-9: a hook this harness will not run is reported, not dropped.
///
/// A deployment whose hook is silently ignored concludes the harness is broken.
/// Naming it is what lets a startup log say so.
#[test]
fn a_hook_that_will_not_run_is_named() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write(
        dir.path(),
        "settings.json",
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"http","url":"https://example.test"}]}]}}"#,
    );

    let found = Discovery::at(&path)
        .load(HookDialect::ClaudeCode)
        .expect("a readable config");

    assert!(found.is_empty(), "nothing runnable was configured");
    assert!(
        found.skipped.iter().any(|s| s.contains("PreToolUse")),
        "but the skip is reported: {:?}",
        found.skipped
    );
}

/// TC-HOOK-DISC-10: text already in hand reaches the same parser.
///
/// A deployment holding its settings somewhere other than a file should not
/// have to write a temporary one to use this crate.
#[test]
fn configuration_already_read_reaches_the_same_parser() {
    let from_text = Discovery::at("in-memory")
        .parse(HookDialect::ClaudeCode, CLAUDE_HOOKS)
        .expect("parsed");
    assert_eq!(from_text.event("PreToolUse").len(), 1);
}

/// TC-HOOK-DISC-11: the conventional path is spelled, not searched for.
///
/// A bridge that went hunting a directory tree and picked up a file nobody
/// pointed it at would be running programs nobody authorised. This only names
/// the candidate; a deployment still has to choose it.
#[test]
fn the_conventional_path_is_spelled_rather_than_hunted() {
    let workspace = Path::new("/srv/app");
    assert_eq!(
        conventional_path(workspace, HookDialect::ClaudeCode),
        Path::new("/srv/app/.claude/settings.json")
    );
    assert_eq!(
        conventional_path(workspace, HookDialect::Codex),
        Path::new("/srv/app/.codex/hooks.json")
    );
}
