//! Test Design Specification: the filesystem service, ported.
//!
//! Feature under test: `tetanus_fs::service::FileSystem` as the two shipped
//! backends implement it - identity, metadata, text reads, guarded and atomic
//! mutations, listings, globs and deletes, each failing in a named class rather
//! than in a message. Upstream pins the same decisions in
//! `packages/fs/fs/tests/service.spec.ts` and
//! `packages/fs/fs-local/tests/filesystem.spec.ts`; each case names the rule it
//! restates.
//!
//! Approach: a real directory on a real disk, through the public trait. A
//! double would test the double: canonicalization, atomic replacement and the
//! codes an operating system reports have no faithful stand-in, and they are
//! the behaviour under test.
//!
//! What is not restated, and why. Upstream's `streamText` and `readBytes` have
//! no counterpart: this service reads text whole under a stated cap, and the
//! window a model sees is the tool layer's. Its `fileUrl` and `lstat` are
//! unrepresentable here for the same reason its `processPath` is not -
//! [`FsTarget::path`] already answers "what may another OS capability open",
//! and no consumer in this workspace asks a URI question. Its Windows ACL half
//! (`win32.spec.ts`) is out of scope for a Unix-hosted gate.
//!
//! Environmental needs: a writable temporary directory. No case reaches a
//! network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod support;

use support::Fixture;
use tetanus_fs::error::FsErrorCode;
use tetanus_fs::service::{EditRequest, FileKind, WriteIntent, MAX_TEXT_BYTES};
use tetanus_fs::{FsMode, FsTarget};

/// TC-PORT-FS-1: two spellings of one file are one identity.
///
/// Upstream: "resolve preserves target identity across aliases".
///
/// Input: the same file named plainly, through `./`, through a `..` that comes
/// back, and through a symlink to it.
/// Expected: one `key` for all four - identity is what a later guard is keyed
/// on, so two spellings that were two identities would let a session write over
/// a file it had read under the other name.
#[test]
fn one_file_resolves_to_one_identity_whatever_it_is_called() {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "fn main() {}\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        fixture.root().join("src/main.rs"),
        fixture.root().join("link.rs"),
    )
    .expect("symlink");
    let fs = fixture.sandboxed();

    let plain = fs.resolve("src/main.rs").expect("plain");
    let dotted = fs.resolve("./src/main.rs").expect("dotted");
    let doubled = fs.resolve("src/../src/main.rs").expect("doubled");

    assert_eq!(plain.key(), dotted.key());
    assert_eq!(plain.key(), doubled.key());
    #[cfg(unix)]
    assert_eq!(
        plain.key(),
        fs.resolve("link.rs").expect("through the link").key(),
        "a symlink is followed before the identity is taken, so the link and its \
         destination are one target"
    );
}

/// TC-PORT-FS-2: a path is shown relative to the workspace.
///
/// Upstream: `displayPath` is separate from the process path.
///
/// Input: a nested file, resolved.
/// Expected: `display` is the workspace-relative spelling and `path` is the
/// absolute one. A transcript carrying the absolute path would leak the home
/// directory of whoever ran the session.
#[test]
fn a_target_shows_a_relative_path_and_opens_an_absolute_one() {
    let fixture = Fixture::new();
    fixture.write("docs/plan.md", "# plan\n");
    let fs = fixture.sandboxed();

    let target = fs.resolve("docs/plan.md").expect("resolve");

    assert_eq!(target.display(), "docs/plan.md");
    assert_eq!(target.path(), fixture.root().join("docs/plan.md"));
}

/// TC-PORT-FS-3: a path that is not there yet still resolves.
///
/// Upstream: resolve is not an existence check; `stat` answers that.
///
/// Input: a file that does not exist, and a stat of it.
/// Expected: resolution succeeds - a create has to be judged before anything is
/// created - and the stat answers `None` rather than an error, because absence
/// is an answer to "what is there" and not a failure of the question.
#[test]
fn a_path_that_does_not_exist_resolves_and_stats_as_absent() {
    let fixture = Fixture::new();
    let fs = fixture.sandboxed();

    let target = fs.resolve("not/there/yet.txt").expect("resolve");

    assert_eq!(target.display(), "not/there/yet.txt");
    assert_eq!(fs.stat(&target).expect("stat"), None);
}

/// TC-PORT-FS-4: reading a file answers its content and the version it had.
///
/// Upstream: `readText` plus the version a guard is later taken against.
///
/// Input: a seeded file, read twice with an edit in between.
/// Expected: the content each time, and a different version after the change -
/// a version that did not move would make every staleness guard useless.
#[test]
fn a_read_answers_the_content_and_a_version_that_moves_with_it() {
    let fixture = Fixture::new();
    fixture.write("notes.txt", "one\n");
    let fs = fixture.sandboxed();
    let target = fs.resolve("notes.txt").expect("resolve");

    let (first, before) = fs.read(&target).expect("first read");
    // Filesystems report mtime at a granularity coarser than this test runs
    // at; the size change is what makes the two versions differ regardless.
    fixture.write("notes.txt", "one\ntwo\n");
    let (second, after) = fs.read(&target).expect("second read");

    assert_eq!(first, "one\n");
    assert_eq!(second, "one\ntwo\n");
    assert_ne!(before, after);
}

/// TC-PORT-FS-5: what cannot be read as text says which class it fell into.
///
/// Upstream: `FS_NOT_TEXT` for undecodable bytes, `FS_NOT_REGULAR_FILE` for a
/// directory, `FS_NOT_FOUND` for absence, `FS_TOO_LARGE` past the cap.
///
/// Input: a file of invalid UTF-8, a directory, and a missing path.
/// Expected: the three distinct codes. A caller routing on the class is the
/// whole reason the service does not answer `io::Error`.
#[test]
fn each_unreadable_thing_reports_its_own_class() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root().join("blob.bin"), [0xff, 0xfe, 0x00]).expect("binary");
    fixture.mkdir("subdir");
    let fs = fixture.sandboxed();

    let binary = fs
        .read(&fs.resolve("blob.bin").expect("resolve"))
        .unwrap_err();
    let directory = fs
        .read(&fs.resolve("subdir").expect("resolve"))
        .unwrap_err();
    let missing = fs
        .read(&fs.resolve("gone.txt").expect("resolve"))
        .unwrap_err();

    assert_eq!(binary.code(), FsErrorCode::NotText);
    assert_eq!(directory.code(), FsErrorCode::NotRegularFile);
    assert_eq!(missing.code(), FsErrorCode::NotFound);
    assert!(
        binary.to_string().contains("blob.bin"),
        "every message names the path it is about: {binary}"
    );
}

/// TC-PORT-FS-6: a file over the cap is refused rather than truncated.
///
/// Upstream: `FS_TOO_LARGE` instead of a truncated result.
///
/// Input: a file one byte past `MAX_TEXT_BYTES`.
/// Expected: `FS_TOO_LARGE`, naming both the size and the limit, and the file
/// untouched. A truncated string returned as the file is the failure mode this
/// exists to prevent: the model would edit against content that is not there.
#[test]
fn a_file_over_the_cap_is_refused_and_says_by_how_much() {
    let fixture = Fixture::new();
    let oversized = "x".repeat(MAX_TEXT_BYTES as usize + 1);
    fixture.write("huge.txt", &oversized);
    let fs = fixture.sandboxed();

    let refused = fs
        .read(&fs.resolve("huge.txt").expect("resolve"))
        .unwrap_err();

    assert_eq!(refused.code(), FsErrorCode::TooLarge);
    let message = refused.to_string();
    assert!(message.contains(&MAX_TEXT_BYTES.to_string()), "{message}");
    assert!(
        message.contains("window"),
        "it says what to do instead: {message}"
    );
}

/// TC-PORT-FS-7: an unconditional write creates, and then updates.
///
/// Upstream: `writeText` with no intent is create-or-overwrite, and the outcome
/// says which it was.
///
/// Input: a write to an absent path, then a write over it.
/// Expected: `Create` with no `before`, then `Update` carrying the prior
/// content - the diff basis a renderer needs, whole rather than as a diff.
#[test]
fn an_unconditional_write_creates_then_updates_and_reports_which() {
    let fixture = Fixture::new();
    let fs = fixture.sandboxed();
    let target = fs.resolve("out/report.md").ok();
    // A write does not create parent directories: the parent must exist, and
    // the case is about the outcome shape, not about directory creation.
    fixture.mkdir("out");
    let target = target.unwrap_or_else(|| fs.resolve("out/report.md").expect("resolve"));

    let created = fs
        .write(&target, "first\n", &WriteIntent::Unconditional)
        .expect("create");
    let updated = fs
        .write(&target, "second\n", &WriteIntent::Unconditional)
        .expect("update");

    assert_eq!(created.operation.as_str(), "create");
    assert_eq!(created.before, None);
    assert_eq!(updated.operation.as_str(), "update");
    assert_eq!(updated.before.as_deref(), Some("first\n"));
    assert_eq!(fixture.read("out/report.md"), "second\n");
    assert_ne!(created.version, updated.version);
}

/// TC-PORT-FS-8: `createIfAbsent` refuses a file that is already there.
///
/// Upstream: "createIfAbsent rejects an existing target with
/// `FS_NOT_OBSERVED`".
///
/// Input: a write under `CreateIfAbsent` over an existing file.
/// Expected: `FS_NOT_OBSERVED`, the file untouched, and a message that says to
/// read it first. The code is upstream's and it is the right one: what is wrong
/// is not the write but that it is blind.
#[test]
fn create_if_absent_refuses_a_file_that_is_already_there() {
    let fixture = Fixture::new();
    fixture.write("kept.txt", "original\n");
    let fs = fixture.sandboxed();
    let target = fs.resolve("kept.txt").expect("resolve");

    let refused = fs
        .write(&target, "clobbered\n", &WriteIntent::CreateIfAbsent)
        .unwrap_err();

    assert_eq!(refused.code(), FsErrorCode::NotObserved);
    assert_eq!(fixture.read("kept.txt"), "original\n");
    assert!(refused.to_string().contains("Read it first"), "{refused}");
}

/// TC-PORT-FS-9: a version guard refuses a file that moved, and absence too.
///
/// Upstream: "replaceIfVersion rejects absence or mismatch with
/// `FS_STALE_VERSION`".
///
/// Input: a write guarded at a version taken before the file changed, and a
/// write guarded at a version for a file that is no longer there.
/// Expected: `FS_STALE_VERSION` both times, the file untouched, and a message
/// telling the caller to read it again - the one move that works.
#[test]
fn a_version_guard_refuses_both_a_changed_file_and_a_missing_one() {
    let fixture = Fixture::new();
    fixture.write("guarded.txt", "v1\n");
    let fs = fixture.sandboxed();
    let target = fs.resolve("guarded.txt").expect("resolve");
    let (_, stale) = fs.read(&target).expect("read");
    fixture.write("guarded.txt", "v2 from somewhere else\n");

    let changed = fs
        .write(
            &target,
            "v3\n",
            &WriteIntent::ReplaceIfVersion(stale.clone()),
        )
        .unwrap_err();
    std::fs::remove_file(fixture.root().join("guarded.txt")).expect("remove");
    let vanished = fs
        .write(&target, "v3\n", &WriteIntent::ReplaceIfVersion(stale))
        .unwrap_err();

    assert_eq!(changed.code(), FsErrorCode::StaleVersion);
    assert_eq!(vanished.code(), FsErrorCode::StaleVersion);
    assert!(changed.to_string().contains("Read it again"), "{changed}");
}

/// TC-PORT-FS-10: a guarded write at the current version goes through.
///
/// Upstream: the guard is a precondition, not a prohibition.
///
/// Input: read, then write guarded at exactly what was read.
/// Expected: the write lands, and the outcome carries a version that is not the
/// one it was guarded at - the file it now describes is the new one.
#[test]
fn a_guarded_write_at_the_current_version_lands() {
    let fixture = Fixture::new();
    fixture.write("guarded.txt", "v1\n");
    let fs = fixture.sandboxed();
    let target = fs.resolve("guarded.txt").expect("resolve");
    let (_, current) = fs.read(&target).expect("read");

    let outcome = fs
        .write(
            &target,
            "v2\n",
            &WriteIntent::ReplaceIfVersion(current.clone()),
        )
        .expect("guarded write");

    assert_eq!(fixture.read("guarded.txt"), "v2\n");
    assert_ne!(outcome.version, current);
}

/// TC-PORT-FS-11: a write is atomic, so a reader never sees a partial file.
///
/// Upstream: "mutations are atomic".
///
/// Input: a write over an existing file, with the directory listed afterwards.
/// Expected: the new content, and no temporary left behind. The publish is a
/// rename, so a reader holds either the whole old file or the whole new one;
/// what a test can observe of that is that nothing else is in the directory.
#[test]
fn a_write_publishes_by_rename_and_leaves_no_temporary_behind() {
    let fixture = Fixture::new();
    fixture.write("data.json", "{}\n");
    let fs = fixture.sandboxed();
    let target = fs.resolve("data.json").expect("resolve");

    fs.write(&target, "{\"ok\":true}\n", &WriteIntent::Unconditional)
        .expect("write");

    let names: Vec<String> = fs
        .list(&fs.resolve(".").expect("root"))
        .expect("list")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, vec!["data.json".to_string()]);
    assert_eq!(fixture.read("data.json"), "{\"ok\":true}\n");
}

/// TC-PORT-FS-12: a literal edit replaces one occurrence.
///
/// Upstream: `editText` matches literally and rewrites in one critical section.
///
/// Input: a file with one occurrence of the text, edited.
/// Expected: the replacement, `before` and `after` carried whole, and one
/// replacement counted.
#[test]
fn an_edit_replaces_the_one_occurrence_and_carries_both_sides() {
    let fixture = Fixture::new();
    fixture.write("code.rs", "let x = 1;\nlet y = 2;\n");
    let fs = fixture.sandboxed();
    let target = fs.resolve("code.rs").expect("resolve");

    let outcome = fs
        .edit(
            &target,
            &EditRequest {
                old: "let y = 2;".into(),
                new: "let y = 3;".into(),
                replace_all: false,
            },
            None,
        )
        .expect("edit");

    assert_eq!(outcome.replacements, 1);
    assert_eq!(outcome.before, "let x = 1;\nlet y = 2;\n");
    assert_eq!(outcome.after, "let x = 1;\nlet y = 3;\n");
    assert_eq!(fixture.read("code.rs"), "let x = 1;\nlet y = 3;\n");
}

/// TC-PORT-FS-13: an ambiguous edit changes nothing and says how ambiguous.
///
/// Upstream: `FS_AMBIGUOUS_EDIT` unless `replaceAll`, and `FS_EDIT_NOT_FOUND`
/// when the text is absent.
///
/// Input: text occurring three times, edited without and then with
/// `replace_all`; and text that does not occur at all.
/// Expected: the count in the refusal - a model told "occurs 3 times" can add
/// context, one told "ambiguous" cannot - the file untouched, then all three
/// replaced when asked for, and a distinct class for text that is simply not
/// there.
#[test]
fn an_ambiguous_edit_is_refused_with_its_count_and_leaves_the_file_alone() {
    let fixture = Fixture::new();
    fixture.write("repeat.txt", "a\na\na\n");
    let fs = fixture.sandboxed();
    let target = fs.resolve("repeat.txt").expect("resolve");
    let request = |replace_all| EditRequest {
        old: "a".into(),
        new: "b".into(),
        replace_all,
    };

    let refused = fs.edit(&target, &request(false), None).unwrap_err();
    let untouched = fixture.read("repeat.txt");
    let replaced = fs.edit(&target, &request(true), None).expect("replace all");
    let absent = fs
        .edit(
            &target,
            &EditRequest {
                old: "nowhere".into(),
                new: "x".into(),
                replace_all: false,
            },
            None,
        )
        .unwrap_err();

    assert_eq!(refused.code(), FsErrorCode::AmbiguousEdit);
    assert!(refused.to_string().contains("3 times"), "{refused}");
    assert_eq!(untouched, "a\na\na\n");
    assert_eq!(replaced.replacements, 3);
    assert_eq!(fixture.read("repeat.txt"), "b\nb\nb\n");
    assert_eq!(absent.code(), FsErrorCode::EditNotFound);
}

/// TC-PORT-FS-14: an empty needle is refused before anything is matched.
///
/// Upstream: `oldString` is "literal non-empty text".
///
/// Input: an edit whose text to replace is the empty string.
/// Expected: refused, with a reason saying why an empty needle cannot mean
/// anything. Left to the matcher it would occur between every pair of
/// characters, and `replace_all` would rewrite the file into noise.
#[test]
fn an_empty_needle_is_refused_rather_than_matched_everywhere() {
    let fixture = Fixture::new();
    fixture.write("code.rs", "fn main() {}\n");
    let fs = fixture.sandboxed();
    let target = fs.resolve("code.rs").expect("resolve");

    let refused = fs
        .edit(
            &target,
            &EditRequest {
                old: String::new(),
                new: "x".into(),
                replace_all: true,
            },
            None,
        )
        .unwrap_err();

    assert_eq!(refused.code(), FsErrorCode::BadPattern);
    assert_eq!(fixture.read("code.rs"), "fn main() {}\n");
}

/// TC-PORT-FS-15: an edit under a stale guard refuses before it matches.
///
/// Upstream: "the version guard is checked before matching so stale content
/// reports `FS_STALE_VERSION`".
///
/// Input: a guard taken before the file changed, with text that *does* occur in
/// the new content.
/// Expected: `FS_STALE_VERSION`, not a successful edit. Order matters: matching
/// first would have succeeded and quietly written over somebody else's change.
#[test]
fn a_stale_guard_refuses_an_edit_whose_text_would_have_matched() {
    let fixture = Fixture::new();
    fixture.write("shared.txt", "hello\n");
    let fs = fixture.sandboxed();
    let target = fs.resolve("shared.txt").expect("resolve");
    let (_, stale) = fs.read(&target).expect("read");
    fixture.write("shared.txt", "hello from somebody else\n");

    let refused = fs
        .edit(
            &target,
            &EditRequest {
                old: "hello".into(),
                new: "goodbye".into(),
                replace_all: false,
            },
            Some(&stale),
        )
        .unwrap_err();

    assert_eq!(refused.code(), FsErrorCode::StaleVersion);
    assert_eq!(fixture.read("shared.txt"), "hello from somebody else\n");
}

/// TC-PORT-FS-16: a listing is metadata in name order, and content-free.
///
/// Upstream: "listing returns metadata and resolved targets only; it must not
/// read file contents", "in stable name order".
///
/// Input: a directory holding files and a subdirectory, seeded out of order.
/// Expected: entries sorted by name, each with its kind and a target that
/// resolves, and `FS_NOT_DIRECTORY` when the listed path is a file.
#[test]
fn a_listing_is_sorted_metadata_with_resolved_children() {
    let fixture = Fixture::new();
    fixture.write("zeta.txt", "z");
    fixture.write("alpha.txt", "aa");
    fixture.mkdir("middle");
    let fs = fixture.sandboxed();

    let entries = fs.list(&fs.resolve(".").expect("root")).expect("list");
    let refused = fs
        .list(&fs.resolve("alpha.txt").expect("resolve"))
        .unwrap_err();

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["alpha.txt", "middle", "zeta.txt"]);
    assert_eq!(entries[0].kind, FileKind::File);
    assert_eq!(entries[0].size, 2);
    assert_eq!(entries[1].kind, FileKind::Directory);
    assert_eq!(entries[0].target.display(), "alpha.txt");
    assert_eq!(refused.code(), FsErrorCode::NotDirectory);
}

/// TC-PORT-FS-17: a glob answers matches in stable order and nothing else.
///
/// Upstream: `packages/fs/tool-fs-search`, restated in-process.
///
/// Input: a tree of Rust and Markdown files, matched by three patterns.
/// Expected: `**` crossing directories, `*` staying inside one name, and a
/// pattern that matches nothing answering an empty list rather than an error -
/// "nothing matched" is a fact, and a model handed an error would retry it.
#[test]
fn a_glob_matches_across_directories_and_answers_nothing_calmly() {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", "");
    fixture.write("src/deep/inner.rs", "");
    fixture.write("README.md", "");
    let fs = fixture.sandboxed();
    let root = fs.resolve(".").expect("root");
    let found = |pattern: &str| -> Vec<String> {
        fs.glob(&root, pattern)
            .expect("glob")
            .into_iter()
            .map(|t| t.display().to_string())
            .collect()
    };

    assert_eq!(found("**/*.rs"), ["src/deep/inner.rs", "src/main.rs"]);
    assert_eq!(found("src/*.rs"), ["src/main.rs"]);
    assert_eq!(found("*.md"), ["README.md"]);
    assert!(found("**/*.toml").is_empty());
}

/// TC-PORT-FS-18: a delete removes a file, and refuses a full directory until
/// it is told to.
///
/// tetanus's own: upstream's service has no delete, so the error class and the
/// refusal are named here rather than restated.
///
/// Input: a file deleted; a directory with content deleted without and then
/// with `recursive`.
/// Expected: the file gone; `FS_DIRECTORY_NOT_EMPTY` with the directory intact;
/// then the directory and its contents gone, with the count reported. A
/// recursive delete that happened by default is the one filesystem mistake a
/// session cannot undo.
#[test]
fn a_delete_takes_a_file_and_refuses_a_full_directory_until_told() {
    let fixture = Fixture::new();
    fixture.write("scratch.txt", "temporary\n");
    fixture.write("tree/inner/leaf.txt", "leaf\n");
    let fs = fixture.sandboxed();

    let file = fs
        .delete(&fs.resolve("scratch.txt").expect("resolve"), false)
        .expect("delete file");
    let tree = fs.resolve("tree").expect("resolve");
    let refused = fs.delete(&tree, false).unwrap_err();
    // Asserted before the recursive delete runs: what the refusal promises is
    // that the tree is still there, and a check after the second call could
    // not tell that apart from the second call having put it back.
    assert_eq!(refused.code(), FsErrorCode::DirectoryNotEmpty);
    assert!(fixture.exists("tree/inner/leaf.txt"));
    let recursive = fs.delete(&tree, true).expect("recursive delete");

    assert_eq!(file.kind, FileKind::File);
    assert!(!fixture.exists("scratch.txt"));
    assert_eq!(recursive.kind, FileKind::Directory);
    assert_eq!(
        recursive.entries, 3,
        "the directory, the subdirectory, the leaf"
    );
    assert!(!fixture.exists("tree"));
}

/// TC-PORT-FS-19: the local backend answers the same questions, unfenced.
///
/// Upstream: the bare local backend "reports `undefined`" for its sandbox mode
/// and confines nothing.
///
/// Input: the unfenced backend, asked for a path outside its working
/// directory, and asked to write there.
/// Expected: both succeed, and the backend reports `danger-full-access`. The
/// two backends differing in exactly this is what makes the fence a
/// composition choice rather than something buried in an implementation.
#[test]
fn the_local_backend_reaches_outside_its_working_directory() {
    let fixture = Fixture::new();
    let outside = fixture.outside().join("elsewhere.txt");
    let fs = fixture.local();

    let target = fs.resolve(&outside.display().to_string()).expect("resolve");
    fs.write(&target, "written outside\n", &WriteIntent::Unconditional)
        .expect("write outside");

    assert_eq!(fs.mode(), FsMode::DangerFullAccess);
    assert_eq!(fs.backend(), "local");
    assert_eq!(
        std::fs::read_to_string(&outside).expect("read back"),
        "written outside\n"
    );
}

/// TC-PORT-FS-20: a target is never manufactured by a consumer.
///
/// Upstream: "a consumer never manufactures a key, it receives one from
/// `resolve()`".
///
/// Input: a target built by hand naming a path outside the workspace, handed to
/// the fenced backend.
/// Expected: the operation happens, and that is precisely why the constructor
/// is documented as backend-only. This case pins the boundary rather than a
/// guarantee: the fence lives at `resolve`, and a caller that skips resolution
/// has skipped the fence. Naming it here means the next reader learns it from a
/// case instead of from an incident.
#[test]
fn a_hand_built_target_bypasses_the_fence_which_is_why_resolution_owns_it() {
    let fixture = Fixture::new();
    let outside = fixture.outside().join("smuggled.txt");
    let fs = fixture.sandboxed();

    let forged = FsTarget::new(
        outside.display().to_string(),
        "smuggled.txt",
        outside.clone(),
    );
    let wrote = fs.write(&forged, "smuggled\n", &WriteIntent::Unconditional);

    assert!(
        wrote.is_ok(),
        "the fence is at resolve; this documents that skipping it skips the fence"
    );
    assert!(
        fs.resolve(&outside.display().to_string()).is_err(),
        "the supported route is refused, which is the route every tool takes"
    );
}
