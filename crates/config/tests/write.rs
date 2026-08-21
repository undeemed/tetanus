//! Test Design Specification: writing the settings document back, ported.
//!
//! Feature under test: `tetanus_config::write` - applying edits to the document
//! on disk, what it refuses, and how the file is replaced. Upstream pins the
//! same decisions in the persist half of
//! `packages/settings/settings-file/tests/local.spec.ts` (`update`, `mutate`,
//! `publish`).
//!
//! Approach: real files in a temporary directory, read back through
//! `tetanus_config::file::read` - the reader the rest of the harness uses. A
//! case that asserted the bytes written would be asserting a serializer's
//! formatting; what matters is that what was written reads back as what was
//! asked for.
//!
//! What is not restated, and why. Upstream writes through a comment-preserving
//! YAML editor; reproducing that needs a round-tripping parser this workspace
//! does not have, so TC-PORT-WRITE-7 pins the loss rather than pretending it
//! does not happen, and `docs/parity-updates/` carries it. Upstream's
//! revisions and conflict detection belong to a settings *service* that also
//! publishes to subscribers; this is the file half.
//!
//! Environmental needs: a writable temporary directory. The permissions case is
//! Unix-only and compiles out elsewhere.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;
use tetanus_config::write::{update, Edit};
use tetanus_config::ConfigError;

fn read(path: &Path) -> tetanus_config::Document {
    tetanus_config::file::read(path).expect("the written document reads back")
}

/// TC-PORT-WRITE-1: a first write creates the document and its directories.
///
/// Upstream creates a missing settings home on write.
///
/// Input: an edit against a path two directories below anything that exists.
/// Expected: the file is there and holds the key, nested the way a document
/// nests. A surface offering "remember this" should not have to tell a user to
/// create a directory first.
#[test]
fn a_first_write_creates_the_document_and_its_directories() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("home/config/settings.yaml");

    let written = update(&path, &[Edit::set("llm.model", "deepseek-chat")]).expect("written");

    assert!(path.is_file());
    assert_eq!(written["llm.model"], json!("deepseek-chat"));
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.contains("llm:"), "it is written as a document: {text}");
    assert!(text.contains("model:"), "{text}");
}

/// TC-PORT-WRITE-2: an edit changes one key and leaves the rest of the document
/// alone.
///
/// Upstream's `update` is a read-modify-write of the whole document.
///
/// Input: a document with two sections; one leaf of one section rewritten.
/// Expected: that leaf changed, every other key untouched. A write that
/// rewrote a section whole would silently drop the settings beside the one the
/// caller named.
#[test]
fn an_edit_changes_one_key_and_leaves_the_rest() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    std::fs::write(
        &path,
        "llm:\n  model: old\n  retry:\n    max_retries: 3\nlog:\n  level: debug\n",
    )
    .expect("seed");

    update(&path, &[Edit::set("llm.model", "new")]).expect("written");

    let document = read(&path);
    assert_eq!(document["llm.model"], json!("new"));
    assert_eq!(document["llm.retry.max_retries"], json!(3));
    assert_eq!(document["log.level"], json!("debug"));
}

/// TC-PORT-WRITE-3: several edits are applied in order, and a removal takes the
/// key out.
///
/// Upstream's `mutate` applies a batch.
///
/// Input: one call setting two keys and removing a third, where the second
/// edit overwrites the first.
/// Expected: the later write wins, the removed key is gone, and the document
/// reads back with exactly the keys that should be there. Removing a key that
/// was not there is not an error: the caller asked for it to be gone, and it
/// is.
#[test]
fn a_batch_applies_in_order_and_a_removal_takes_the_key_out() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    std::fs::write(&path, "log:\n  level: debug\n  colour: always\n").expect("seed");

    let written = update(
        &path,
        &[
            Edit::set("log.level", "info"),
            Edit::set("log.level", "warn"),
            Edit::remove("log.colour"),
            Edit::remove("log.never_existed"),
        ],
    )
    .expect("written");

    assert_eq!(written["log.level"], json!("warn"));
    assert!(!written.contains_key("log.colour"));
    assert_eq!(read(&path).len(), 1);
}

/// TC-PORT-WRITE-4: writing through a scalar is refused, not silently
/// discarded.
///
/// The write-side of the rule `crates/config/src/schema.rs` applies when
/// reading.
///
/// Input: a document holding `llm: off`, and an edit setting `llm.model`.
/// Expected: refused, naming `llm`, with the document byte-for-byte unchanged.
/// Creating the section would have thrown away the `off` the user wrote, which
/// is the one case where a write destroys something.
#[test]
fn writing_through_a_scalar_is_refused_and_changes_nothing() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    std::fs::write(&path, "llm: off\n").expect("seed");

    let refused = update(&path, &[Edit::set("llm.model", "deepseek-chat")]).expect_err("refused");

    assert!(
        matches!(&refused, ConfigError::SectionExpected { key, .. } if key == "llm"),
        "said: {refused}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "llm: off\n",
        "the document is untouched"
    );
}

/// TC-PORT-WRITE-5: a document that cannot be parsed is not overwritten.
///
/// Upstream refuses to persist over a document it could not read.
///
/// Input: a file of broken YAML, and an edit.
/// Expected: refused as malformed, and the broken text still on disk. A write
/// path that started from an empty document when it could not parse one would
/// turn a typo into the loss of every setting the user had.
#[test]
fn a_document_that_does_not_parse_is_left_alone() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    let broken = "llm:\n  model: [unclosed\n";
    std::fs::write(&path, broken).expect("seed");

    let refused = update(&path, &[Edit::set("log.level", "debug")]).expect_err("refused");

    assert!(
        matches!(refused, ConfigError::Malformed { .. }),
        "{refused}"
    );
    assert_eq!(std::fs::read_to_string(&path).expect("read"), broken);
}

/// TC-PORT-WRITE-6: the replace is atomic and leaves no temporary behind, and
/// the file is owner-only.
///
/// Upstream: owner-only permissions and an atomic replace.
///
/// Input: two successive writes to one document.
/// Expected: the directory holds exactly the document afterwards, and its mode
/// is 0600. A settings document may hold a credential, so a file that briefly
/// existed at 0644 has already published it - which is why the mode is set on
/// the temporary before the content goes in rather than on the destination
/// afterwards.
#[test]
fn the_replace_is_atomic_and_the_file_is_owner_only() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");

    update(
        &path,
        &[Edit::set("llm.providers.deepseek.api_key", "sk-1")],
    )
    .expect("first");
    update(&path, &[Edit::set("llm.model", "deepseek-chat")]).expect("second");

    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .expect("listing")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries, ["settings.yaml"], "no temporary was left behind");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a document may hold a credential");
    }
}

/// TC-PORT-WRITE-7: a written document keeps its data and loses its comments.
///
/// Upstream writes through a comment-preserving editor; this does not, and the
/// cost is pinned rather than left for a user to discover.
///
/// Input: a commented document, edited.
/// Expected: every key survives with its value, and the comment is gone. Stated
/// as a case so the day somebody adds a round-tripping parser, the case that
/// changes is the one that documented the gap.
#[test]
fn a_written_document_keeps_its_data_and_loses_its_comments() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    std::fs::write(
        &path,
        "# the model this deployment uses\nllm:\n  model: old  # inline\nlog:\n  level: debug\n",
    )
    .expect("seed");

    update(&path, &[Edit::set("llm.model", "new")]).expect("written");

    let text = std::fs::read_to_string(&path).expect("read");
    let document = read(&path);
    assert_eq!(document["llm.model"], json!("new"));
    assert_eq!(document["log.level"], json!("debug"), "the data survives");
    assert!(!text.contains('#'), "the commentary does not: {text}");
}

/// TC-PORT-WRITE-8: JSON stays JSON, and an extension nothing reads is refused.
///
/// Upstream keeps the document in the format it found.
///
/// Input: a `.json` document edited; then an edit against a `.txt` path.
/// Expected: the JSON document is still JSON and reads back through the same
/// reader; the unsupported extension is refused before anything is written. A
/// write path that accepted more than the read path would produce files the
/// harness then refused to load.
#[test]
fn json_stays_json_and_an_unreadable_extension_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let json_path = dir.path().join("settings.json");
    std::fs::write(&json_path, r#"{"log": {"level": "debug"}}"#).expect("seed");
    let wrong = dir.path().join("settings.txt");

    update(&json_path, &[Edit::set("log.level", "warn")]).expect("written");
    let refused = update(&wrong, &[Edit::set("log.level", "warn")]).expect_err("refused");

    let text = std::fs::read_to_string(&json_path).expect("read");
    assert!(text.trim_start().starts_with('{'), "{text}");
    assert_eq!(read(&json_path)["log.level"], json!("warn"));
    assert!(matches!(refused, ConfigError::UnsupportedExtension { .. }));
    assert!(!wrong.exists(), "nothing was written");
}

/// TC-PORT-WRITE-9: what a write answers is what a read of the file would say.
///
/// The property that lets a caller load the result into its `File` layer
/// without reading the file again and racing itself.
///
/// Input: a document with several sections, edited.
/// Expected: the answer equals `file::read` of the same path, key for key. Two
/// paths to the same fact that could disagree is a bug waiting for a
/// deployment to find.
#[test]
fn the_answer_equals_what_a_reader_would_read() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("settings.yaml");
    std::fs::write(&path, "log:\n  level: debug\n").expect("seed");

    let answered = update(
        &path,
        &[
            Edit::set("llm.retry.max_retries", 5),
            Edit::set("llm.retry.codes", json!(["RATE_LIMIT"])),
            Edit::set("agent.max_steps", 12),
        ],
    )
    .expect("written");

    assert_eq!(answered, read(&path));
    assert_eq!(answered["llm.retry.codes"], json!(["RATE_LIMIT"]));
    assert_eq!(answered["agent.max_steps"], json!(12));
}
