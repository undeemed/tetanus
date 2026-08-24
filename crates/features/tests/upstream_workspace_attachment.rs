//! Test Design Specification: the workspace sketch and attachment admission,
//! ported.
//!
//! Features under test: `tetanus_features::workspace` - where the project
//! starts, what is in it, and what it has written down; and
//! `tetanus_features::attachment` - what a deployment admits, what it stores
//! once, and what it records. Upstream pins them in
//! `packages/workspace/workspace/tests/workspace.spec.ts` and
//! `packages/attachment/*/tests/`.
//!
//! Approach: real directories and real bytes. Root discovery is a question
//! about a filesystem, and the image cases are about what a header says versus
//! what a file claims - neither has a faithful double.
//!
//! What is not restated, and why. Most of upstream's workspace package is a
//! *registry* for a picker: persisted order, bootstrap from session headers,
//! titles, cwd-drift grouping, rollback of a provisional cache entry. That is a
//! surface's state over a store rather than something a turn reads, and
//! `docs/parity.md` names it. Upstream's attachment store is
//! transactional over a durable object store with reference metadata and
//! cancellation; this stores files under a session directory, so its
//! cancellation, reference-mismatch and domain-close cases have nothing to
//! restate. Its base64 wire encoding belongs to the boundary, which by
//! `docs/interface-contract.md` §5 is not this lane's to change.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod support;

use std::path::{Path, PathBuf};

use serde_json::json;
use support::Fixture;
use tempfile::TempDir;
use tetanus_features::attachment::{
    address, admit, attach, measure, read, recorded, topic, AdmissionError, Dimensions, Incoming,
    Limits, StoreError,
};
use tetanus_features::workspace::{describe, find_root, WorkspaceError, WorkspaceInfoTool};
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

    fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(&path, content).expect("write");
        path
    }

    fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.root.join(relative);
        std::fs::create_dir_all(&path).expect("dirs");
        path
    }
}

/// A PNG header declaring a size, with no image data behind it.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes
}

fn gif(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&[0x00, 0x00, 0x00]);
    bytes
}

fn jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
    bytes.extend_from_slice(b"JFIF\0");
    bytes.extend_from_slice(&[0; 9]);
    // One start-of-frame segment: marker, length, precision, height, width.
    bytes.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    bytes
}

fn image(name: &str, bytes: Vec<u8>) -> Incoming {
    Incoming {
        name: name.into(),
        media_type: "image/png".into(),
        bytes,
    }
}

fn text(name: &str, body: &str) -> Incoming {
    Incoming {
        name: name.into(),
        media_type: "text/plain".into(),
        bytes: body.as_bytes().to_vec(),
    }
}

/// TC-PORT-WS-1: the root is the nearest marker above the working directory.
///
/// Upstream anchors a workspace on a canonical project path; tetanus finds it
/// with the marker list the instruction search already walks to.
///
/// Input: a checkout with a `.git` marker, described from three directories
/// down.
/// Expected: the root is the marked directory, the working directory is
/// reported separately, and the marker is named. A root that were wherever the
/// process started would make every relative path mean something different per
/// session.
#[test]
fn the_root_is_the_nearest_marker_above_the_working_directory() {
    let tree = Tree::new();
    tree.mkdir("project/.git");
    let deep = tree.mkdir("project/crates/parser/src");

    let (root, marker) = find_root(&deep);
    let described = describe(&deep).expect("described");

    assert_eq!(root, tree.root.join("project"));
    assert_eq!(marker.as_deref(), Some(".git"));
    assert_eq!(described.root, tree.root.join("project"));
    assert_eq!(described.cwd, deep);
}

/// TC-PORT-WS-2: with no marker anywhere, the working directory stands in and
/// says so.
///
/// Upstream requires a canonical directory and nothing more.
///
/// Input: a plain directory with no marker above it inside the fixture.
/// Expected: the root is the directory itself, no marker is named, and the
/// rendering says the working directory is standing in. "This is a project" and
/// "this is a directory" are different facts, and a model told the first about
/// the second will look for things that are not there.
#[test]
fn with_no_marker_the_working_directory_stands_in_and_the_sketch_says_so() {
    let tree = Tree::new();
    let plain = tree.mkdir("just-a-directory");
    tree.write("just-a-directory/notes.txt", "hello");

    let described = describe(&plain).expect("described");

    assert_eq!(described.root, plain);
    // The fixture lives under a temporary directory, which on a developer's
    // machine may itself sit inside a checkout; the case asserts the reported
    // marker only when the walk genuinely found none.
    if described.marker.is_none() {
        assert!(
            described.render().contains("No repository marker"),
            "{}",
            described.render()
        );
    }
}

/// TC-PORT-WS-3: a path that is not a directory is refused, and so is one that
/// is not there.
///
/// Upstream: "rejects nonexistent and non-directory paths without changing
/// order".
///
/// Input: a file, and a path with nothing at it.
/// Expected: two distinct refusals. A workspace that answered "nothing here"
/// for a file would have told the model something false.
#[test]
fn a_file_and_a_missing_path_are_both_refused_and_differently() {
    let tree = Tree::new();
    let file = tree.write("README.md", "# hi");

    let not_a_directory = describe(&file).expect_err("refused");
    let missing = describe(&tree.root.join("nowhere")).expect_err("refused");

    assert!(matches!(
        not_a_directory,
        WorkspaceError::NotADirectory { .. }
    ));
    assert!(matches!(missing, WorkspaceError::Missing { .. }));
}

/// TC-PORT-WS-4: the sketch names the top level and the instruction files, and
/// leaves machinery out.
///
/// The brief's "roots, layout, and the instructions file", and upstream's
/// canonical-path rule.
///
/// Input: a project with source directories, a README, a dot directory and an
/// `AGENTS.md`.
/// Expected: directories before files, each in name order, the dot entry
/// omitted, and the instruction file named the way the prompt names it. Naming
/// forty dot entries crowds out the twelve that say what the project is.
#[test]
fn the_sketch_lists_the_top_level_and_the_instruction_files() {
    let tree = Tree::new();
    tree.mkdir("project/.git");
    tree.mkdir("project/src");
    tree.mkdir("project/docs");
    tree.write("project/README.md", "# project");
    tree.write("project/AGENTS.md", "Work carefully.");
    let project = tree.root.join("project");

    let described = describe(&project).expect("described");
    let rendered = described.render();

    let names: Vec<&str> = described
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, ["docs", "src", "AGENTS.md", "README.md"]);
    assert!(!names.contains(&".git"));
    assert_eq!(described.instructions, ["AGENTS.md"]);
    assert!(rendered.contains("  docs/\n"), "{rendered}");
    assert!(rendered.contains("Instruction files"), "{rendered}");
}

/// TC-PORT-WS-5: the tool answers the sketch, and a gone directory is a result
/// rather than a failure.
///
/// Upstream surfaces a workspace through its own read path.
///
/// Input: the tool over a real project; then over a path that was removed.
/// Expected: `ok` with the root named; then a failed result rather than a
/// failed step - the working directory being gone is something the model can
/// work around by naming absolute paths, and failing the step would only end
/// the turn.
#[tokio::test]
async fn the_tool_answers_the_sketch_and_survives_a_missing_directory() {
    let tree = Tree::new();
    tree.mkdir("project/.git");
    tree.write("project/Cargo.toml", "[package]");
    let project = tree.root.join("project");
    let gone = tree.root.join("gone");

    let described = WorkspaceInfoTool::new(&project)
        .execute(&json!({}))
        .await
        .expect("ran");
    let missing = WorkspaceInfoTool::new(&gone)
        .execute(&json!({}))
        .await
        .expect("ran");

    assert!(described.ok);
    assert!(
        described.content.contains("Project root:"),
        "{}",
        described.content
    );
    assert!(
        described.content.contains("Cargo.toml"),
        "{}",
        described.content
    );
    assert!(!missing.ok);
    assert!(
        missing.content.contains("nothing at this path"),
        "{}",
        missing.content
    );
}

/// TC-PORT-ATTACH-1: the whole batch is judged before anything is stored.
///
/// Upstream: "validates the complete batch before saving in input order" and
/// "starts no writes when any member fails validation".
///
/// Input: a batch whose second member is empty.
/// Expected: refused, and the store directory holds nothing - not even the
/// first member, which was fine. A half-admitted batch is the worst outcome
/// available: the turn sees part of what was attached and nobody can tell which
/// part is missing.
#[tokio::test]
async fn one_bad_member_stores_none_of_the_batch() {
    let h = Fixture::new("batch").await;
    let tree = Tree::new();
    let store = tree.root.join("objects");
    let batch = vec![
        text("good.txt", "fine"),
        Incoming {
            name: "empty.txt".into(),
            media_type: "text/plain".into(),
            bytes: Vec::new(),
        },
    ];

    let refused =
        attach(h.log().as_ref(), &store, &batch, &Limits::default()).expect_err("refused");

    assert!(matches!(
        refused,
        StoreError::Refused(AdmissionError::Empty { .. })
    ));
    assert!(!store.exists(), "no object was written");
    assert!(h.events(topic::ATTACHMENT_ADDED).is_empty());
}

/// TC-PORT-ATTACH-2: the batch-wide limits are reported before the per-item
/// ones.
///
/// Upstream: "rejects count, aggregate bytes, and deployment media types before
/// validation".
///
/// Input: a batch over the count limit whose members are also individually too
/// large.
/// Expected: the count refusal. A caller who attached forty files should be told
/// that, rather than being told about the first file and discovering the count
/// limit on the next attempt.
#[test]
fn the_batch_limits_are_reported_before_the_item_limits() {
    let limits = Limits {
        max_items: 2,
        max_item_bytes: 4,
        ..Limits::default()
    };
    let batch = vec![
        text("a.txt", "far too long"),
        text("b.txt", "far too long"),
        text("c.txt", "far too long"),
    ];

    let refused = admit(&batch, &limits).expect_err("refused");

    assert_eq!(refused, AdmissionError::TooMany { count: 3, limit: 2 });
}

/// TC-PORT-ATTACH-3: each limit refuses what it is for, and says the numbers.
///
/// Upstream's admission limits, each resolved explicitly.
///
/// Input: one oversized item; a batch under the item limit but over the
/// aggregate; and a media type the deployment does not admit.
/// Expected: three distinct refusals carrying the measured value and the limit.
/// A refusal that says only "too large" leaves the caller guessing what would
/// fit.
#[test]
fn every_limit_refuses_what_it_is_for_and_names_the_numbers() {
    let limits = Limits {
        max_items: 10,
        max_item_bytes: 8,
        max_total_bytes: 12,
        media_types: vec!["text/plain".into()],
        ..Limits::default()
    };

    let oversized = admit(&[text("big.txt", "nine byte")], &limits).expect_err("refused");
    let aggregate = admit(
        &[text("a.txt", "seven b"), text("b.txt", "seven b")],
        &limits,
    )
    .expect_err("refused");
    let wrong_type = admit(
        &[Incoming {
            name: "x.bin".into(),
            media_type: "application/octet-stream".into(),
            bytes: b"ab".to_vec(),
        }],
        &limits,
    )
    .expect_err("refused");

    assert!(matches!(
        oversized,
        AdmissionError::ItemTooLarge { limit: 8, .. }
    ));
    assert!(matches!(
        aggregate,
        AdmissionError::BatchTooLarge { limit: 12, .. }
    ));
    assert!(matches!(wrong_type, AdmissionError::MediaType { .. }));
    assert!(oversized.to_string().contains("9 bytes"), "{oversized}");
}

/// TC-PORT-ATTACH-4: an image's dimensions come out of its header.
///
/// Upstream: "decodes every supported format and its intrinsic dimensions".
///
/// Input: a PNG, a GIF and a JPEG header declaring known sizes.
/// Expected: the declared dimensions for each. Reading the header is what lets
/// the pixel limit be applied before a decode, which is where a hostile image
/// costs the memory.
#[test]
fn the_dimensions_come_out_of_the_header_for_each_format() {
    assert_eq!(
        measure(&png(1280, 720)),
        Some(Dimensions {
            width: 1280,
            height: 720
        })
    );
    assert_eq!(
        measure(&gif(64, 48)),
        Some(Dimensions {
            width: 64,
            height: 48
        })
    );
    assert_eq!(
        measure(&jpeg(800, 600)),
        Some(Dimensions {
            width: 800,
            height: 600
        })
    );
}

/// TC-PORT-ATTACH-5: an image over the pixel limit is refused before it is
/// decoded, and malformed bytes are refused the same way as an unknown format.
///
/// Upstream: "rejects excess decoded pixels before decoding" and "probes
/// malformed bytes and unsupported formats into the same stable error".
///
/// Input: a 200-byte header declaring 60000x60000, and a file declaring
/// `image/png` whose bytes are not one.
/// Expected: the first is refused for its pixel count with the numbers named;
/// the second is refused as malformed. A caller can do the same thing about a
/// broken file and an unreadable format, so they get the same answer.
#[test]
fn a_huge_declaration_and_a_broken_file_are_both_refused_before_decoding() {
    let limits = Limits {
        max_pixels: 40_000_000,
        ..Limits::default()
    };
    let enormous = image("huge.png", png(60_000, 60_000));
    let broken = image("broken.png", b"not a png at all".to_vec());

    let pixels = admit(&[enormous], &limits).expect_err("refused");
    let malformed = admit(&[broken], &limits).expect_err("refused");

    assert!(matches!(
        pixels,
        AdmissionError::TooManyPixels {
            width: 60_000,
            height: 60_000,
            ..
        }
    ));
    assert!(pixels.to_string().contains("3600000000 pixels"), "{pixels}");
    assert!(matches!(malformed, AdmissionError::Malformed { .. }));
}

/// TC-PORT-ATTACH-6: equal bytes are stored once and addressed the same.
///
/// Upstream: "publishes one private content-addressed object and deduplicates
/// equal bytes".
///
/// Input: the same bytes attached twice under different names, and different
/// bytes attached beside them.
/// Expected: one object per distinct content, two references to the shared one,
/// and the same address for equal bytes every time. The property that matters
/// is not the space saved but that a reference is stable.
#[tokio::test]
async fn equal_bytes_are_stored_once_and_named_the_same() {
    let h = Fixture::new("dedupe").await;
    let tree = Tree::new();
    let store = tree.root.join("objects");
    let batch = vec![
        text("first.txt", "the same bytes"),
        text("second.txt", "the same bytes"),
        text("other.txt", "different bytes"),
    ];

    let admitted = attach(h.log().as_ref(), &store, &batch, &Limits::default()).expect("attached");

    assert_eq!(admitted[0].id, admitted[1].id);
    assert_ne!(admitted[0].id, admitted[2].id);
    assert_eq!(admitted[0].id, address(b"the same bytes"));
    let objects: Vec<_> = std::fs::read_dir(&store)
        .expect("store")
        .flatten()
        .collect();
    assert_eq!(objects.len(), 2, "one object per distinct content");
    assert_eq!(
        read(&store, &admitted[0].id).expect("read"),
        b"the same bytes"
    );
}

/// TC-PORT-ATTACH-7: what was attached is on the journal, and survives a
/// reload.
///
/// Upstream keeps admitted history readable; here the journal is that history.
///
/// Input: a text file and an image attached, then the journal replayed from
/// disk.
/// Expected: one record each, carrying the name, the type, the size and the
/// image's dimensions - and no bytes. A base64 screenshot in a JSONL line is a
/// line no reader can read, and the object store is where the bytes belong.
#[tokio::test]
async fn the_record_says_what_was_attached_and_never_carries_the_bytes() {
    let h = Fixture::new("recorded").await;
    let tree = Tree::new();
    let store = tree.root.join("objects");
    let batch = vec![text("log.txt", "a line"), image("shot.png", png(320, 200))];

    attach(h.log().as_ref(), &store, &batch, &Limits::default()).expect("attached");
    h.flush();
    let replayed = recorded(&h.replay());

    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[0].name, "log.txt");
    assert_eq!(replayed[0].bytes, 6);
    assert_eq!(replayed[0].dimensions, None);
    assert_eq!(
        replayed[1].dimensions,
        Some(Dimensions {
            width: 320,
            height: 200
        })
    );
    let raw = h.events(topic::ATTACHMENT_ADDED);
    assert!(
        raw[1].data.get("bytes").and_then(|v| v.as_u64()).is_some(),
        "`bytes` is a size, not content: {}",
        raw[1].data
    );
}

/// TC-PORT-ATTACH-8: a caller's mistake and a storage fault are different
/// answers.
///
/// Upstream: "separates caller-correctable image admission failures from
/// storage faults".
///
/// Input: an unreadable store directory, with a batch that is otherwise fine.
/// Expected: a storage error rather than a refusal. Collapsing the two would
/// tell a user their screenshot is invalid when the disk is unwritable.
#[cfg(unix)]
#[tokio::test]
async fn a_storage_fault_is_not_reported_as_a_bad_attachment() {
    use std::os::unix::fs::PermissionsExt;

    let h = Fixture::new("fault").await;
    let tree = Tree::new();
    let locked = tree.mkdir("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let store = locked.join("objects");

    let failed = attach(
        h.log().as_ref(),
        &store,
        &[text("fine.txt", "fine")],
        &Limits::default(),
    )
    .expect_err("failed");

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    assert!(
        matches!(failed, StoreError::Storage { .. }),
        "a storage fault, not a refusal: {failed}"
    );
    assert!(h.events(topic::ATTACHMENT_ADDED).is_empty());
}

/// TC-PORT-ATTACH-9: an object already holding different bytes is refused, not
/// overwritten.
///
/// Upstream: "rejects conflicting existing objects".
///
/// Input: an object file planted under an address that does not match its
/// content.
/// Expected: refused as an inconsistent store, and the planted bytes untouched.
/// The address is a deduplication key rather than a cryptographic digest, so
/// the store verifies a hit rather than assuming it - and whichever caller is
/// wrong, silently replacing one's bytes with the other's is worse.
#[tokio::test]
async fn a_conflicting_object_is_refused_rather_than_overwritten() {
    let h = Fixture::new("collision").await;
    let tree = Tree::new();
    let store = tree.mkdir("objects");
    let planted = address(b"the real bytes");
    std::fs::write(store.join(&planted), b"somebody else's bytes").expect("plant");

    let failed = attach(
        h.log().as_ref(),
        &store,
        &[text("real.txt", "the real bytes")],
        &Limits::default(),
    )
    .expect_err("failed");

    assert!(matches!(failed, StoreError::Collision { .. }), "{failed}");
    assert_eq!(
        std::fs::read(store.join(&planted)).expect("still there"),
        b"somebody else's bytes"
    );
}

/// TC-PORT-ATTACH-10: an empty name, and a store rooted where nothing exists
/// yet.
///
/// Upstream: "creates and persists a missing nested home directory against the
/// filesystem root", and validates every scalar.
///
/// Input: an unnamed attachment; then a good one into a store directory two
/// levels below anything that exists.
/// Expected: the first refused; the second creates its directories and stores
/// the object. A deployment should not have to pre-create a path the harness
/// owns.
#[tokio::test]
async fn an_unnamed_attachment_is_refused_and_a_missing_store_root_is_created() {
    let h = Fixture::new("roots").await;
    let tree = Tree::new();
    let nested: PathBuf = tree.root.join("a/b/objects");

    let unnamed = admit(&[text("   ", "body")], &Limits::default()).expect_err("refused");
    let stored = attach(
        h.log().as_ref(),
        &nested,
        &[text("note.txt", "body")],
        &Limits::default(),
    )
    .expect("attached");

    assert!(matches!(unnamed, AdmissionError::Unnamed { .. }));
    assert!(Path::new(&nested).is_dir());
    assert_eq!(read(&nested, &stored[0].id).expect("read"), b"body");
}
