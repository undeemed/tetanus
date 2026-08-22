//! Test Design Specification: the fenced backend and its modes, ported.
//!
//! Feature under test: `tetanus_fs::SandboxedFs` and `tetanus_fs::access` -
//! which paths a tool may see, which it may change, and what a refusal says to
//! the model. Upstream pins the same decisions in
//! `packages/fs/fs-sandbox/tests/fs-sandbox.spec.ts`; the path-containment half
//! of that file is already ported as `crates/turn/tests/upstream_fs_containment.rs`,
//! so what is restated here is the layer over it - the mode vocabulary, the
//! refusal wording, and the rule that a listing never offers what the fence
//! would refuse.
//!
//! Approach: a real workspace with a real sibling directory outside it, and a
//! real symlink between them where the platform has them. Nothing here can be
//! faked: the whole point is what the operating system resolves a path to.
//!
//! What differs from upstream, deliberately. Upstream fences its two mutations
//! and lets every read through; tetanus fences resolution, so a read outside
//! the workspace is refused too. That is strictly narrower and it is one rule
//! rather than two. `docs/parity-updates/` records it. Upstream's
//! `danger-full-access` is a mode of its sandboxing backend; here it selects
//! the unfenced backend instead, so there is no branch inside the fence whose
//! job is to skip the fence.
//!
//! Environmental needs: a writable temporary directory. The symlink cases are
//! Unix-only and compile out elsewhere.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod support;

use support::Fixture;
use tetanus_fs::access::{backend, FsMode};
use tetanus_fs::error::FsErrorCode;
use tetanus_fs::service::{EditRequest, WriteIntent};

/// TC-PORT-FS-21: a path outside the workspace is refused, and the refusal
/// says where the model may work.
///
/// Upstream: `FS_SANDBOX_DENIED` for a write outside the writable roots.
///
/// Input: an absolute path to a sibling directory, and a relative path that
/// climbs out of the workspace.
/// Expected: `FS_SANDBOX_DENIED` both times, naming the workspace root and
/// saying to work inside it. A refusal that only says "denied" leaves the model
/// guessing; this one contains the next move.
#[test]
fn a_path_outside_the_workspace_is_refused_and_names_the_workspace() {
    let fixture = Fixture::new();
    let outside = fixture.outside().join("secrets.env");
    std::fs::write(&outside, "TOKEN=1\n").expect("seed outside");
    let fs = fixture.sandboxed();

    let absolute = fs.resolve(&outside.display().to_string()).unwrap_err();
    let climbing = fs.resolve("../outside/secrets.env").unwrap_err();

    assert_eq!(absolute.code(), FsErrorCode::SandboxDenied);
    assert_eq!(climbing.code(), FsErrorCode::SandboxDenied);
    let message = absolute.to_string();
    assert!(
        message.contains(&fixture.root().display().to_string()),
        "the refusal names the workspace: {message}"
    );
    assert!(
        message.contains("Work inside the workspace"),
        "the refusal says what to do instead: {message}"
    );
}

/// TC-PORT-FS-22: a write outside the workspace never reaches the disk.
///
/// Upstream: the denial happens before the mutation.
///
/// Input: a write to a path outside the workspace, attempted the way a tool
/// attempts it - resolve, then write.
/// Expected: the resolution is refused, and the file is not created. The
/// ordering is the assertion: a fence that refused after writing would be
/// decoration.
#[test]
fn a_write_outside_the_workspace_creates_nothing() {
    let fixture = Fixture::new();
    let outside = fixture.outside().join("escaped.txt");
    let fs = fixture.sandboxed();

    let refused = fs.resolve(&outside.display().to_string()).unwrap_err();

    assert_eq!(refused.code(), FsErrorCode::SandboxDenied);
    assert!(!outside.exists(), "nothing was created outside the fence");
}

/// TC-PORT-FS-23: a symlink out of the workspace is judged by where it goes.
///
/// Upstream: containment resolves before it compares.
///
/// Input: a link inside the workspace pointing at a file outside it.
/// Expected: refused. Comparing the lexical path first would have accepted it,
/// which is the classic escape this ordering exists to close.
#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_workspace_is_refused_on_where_it_points() {
    let fixture = Fixture::new();
    let outside = fixture.outside().join("target.txt");
    std::fs::write(&outside, "outside\n").expect("seed outside");
    std::os::unix::fs::symlink(&outside, fixture.root().join("looks-inside.txt")).expect("symlink");
    let fs = fixture.sandboxed();

    let refused = fs.resolve("looks-inside.txt").unwrap_err();

    assert_eq!(refused.code(), FsErrorCode::SandboxDenied);
}

/// TC-PORT-FS-24: a listing never names a path the fence would refuse.
///
/// tetanus's own consequence of fencing resolution: upstream lists whatever is
/// there because its reads are unfenced.
///
/// Input: a directory holding an ordinary file and a symlink out of the
/// workspace, listed; and the same pair matched by a glob.
/// Expected: the ordinary file in both answers, the escaping link in neither.
/// Offering a path and then refusing it wastes a turn and reads to the model as
/// the harness contradicting itself.
#[cfg(unix)]
#[test]
fn a_listing_and_a_glob_leave_out_what_the_fence_would_refuse() {
    let fixture = Fixture::new();
    let outside = fixture.outside().join("target.txt");
    std::fs::write(&outside, "outside\n").expect("seed outside");
    fixture.write("kept.txt", "inside\n");
    std::os::unix::fs::symlink(&outside, fixture.root().join("escape.txt")).expect("symlink");
    let fs = fixture.sandboxed();
    let root = fs.resolve(".").expect("root");

    let listed: Vec<String> = fs
        .list(&root)
        .expect("list")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    let globbed: Vec<String> = fs
        .glob(&root, "*.txt")
        .expect("glob")
        .into_iter()
        .map(|target| target.display().to_string())
        .collect();

    assert_eq!(listed, ["kept.txt"]);
    assert_eq!(globbed, ["kept.txt"]);
}

/// TC-PORT-FS-25: read-only mode refuses every mutation and permits every
/// read.
///
/// Upstream: "cannot write ...: file access denied under read-only mode".
///
/// Input: a read, a write, an edit and a delete under `read-only`.
/// Expected: the read succeeds; the three mutations are refused with
/// `FS_SANDBOX_DENIED` naming the mode; the files are untouched. The message
/// deliberately does not suggest retrying, because under this mode no retry
/// works.
#[test]
fn read_only_mode_refuses_every_mutation_and_permits_reads() {
    let fixture = Fixture::new();
    fixture.write("kept.txt", "original\n");
    let fs = fixture.in_mode(FsMode::ReadOnly);
    let target = fs.resolve("kept.txt").expect("resolve");

    let (content, _) = fs.read(&target).expect("read is permitted");
    let wrote = fs
        .write(&target, "changed\n", &WriteIntent::Unconditional)
        .unwrap_err();
    let edited = fs
        .edit(
            &target,
            &EditRequest {
                old: "original".into(),
                new: "changed".into(),
                replace_all: false,
            },
            None,
        )
        .unwrap_err();
    let deleted = fs.delete(&target, false).unwrap_err();

    assert_eq!(content, "original\n");
    for refused in [&wrote, &edited, &deleted] {
        assert_eq!(refused.code(), FsErrorCode::SandboxDenied);
        assert!(
            refused.to_string().contains("read-only mode"),
            "the refusal names the mode: {refused}"
        );
    }
    assert_eq!(fixture.read("kept.txt"), "original\n");
    assert_eq!(fs.mode(), FsMode::ReadOnly);
}

/// TC-PORT-FS-26: the mode chooses the backend, and the words are upstream's.
///
/// Upstream: `SANDBOX_MODES` is `read-only`, `workspace-write`,
/// `danger-full-access`.
///
/// Input: each mode passed to the composition function, and a word that is not
/// a mode.
/// Expected: the two confining modes answer the fenced backend and
/// `danger-full-access` answers the unfenced one; an unknown word is refused
/// rather than defaulted, because guessing which mode a deployment meant is
/// wrong in both directions.
#[test]
fn a_mode_chooses_a_backend_and_an_unknown_word_is_refused() {
    let fixture = Fixture::new();

    let read_only = backend(FsMode::ReadOnly, fixture.root()).expect("read-only backend");
    let workspace = backend(FsMode::WorkspaceWrite, fixture.root()).expect("workspace backend");
    let full = backend(FsMode::DangerFullAccess, fixture.root()).expect("unfenced backend");

    assert_eq!(read_only.backend(), "sandboxed");
    assert_eq!(workspace.backend(), "sandboxed");
    assert_eq!(full.backend(), "local");
    assert_eq!(
        FsMode::parse("workspace-write").expect("known"),
        FsMode::WorkspaceWrite
    );
    assert_eq!(
        FsMode::default(),
        FsMode::WorkspaceWrite,
        "a deployment that says nothing is fenced"
    );
    let unknown = FsMode::parse("yolo").unwrap_err();
    assert!(unknown.to_string().contains("yolo"), "{unknown}");
}

/// TC-PORT-FS-27: the workspace root itself cannot be deleted.
///
/// tetanus's own: upstream has no delete, and this is the one delete that would
/// leave every later operation failing on a root that is gone.
///
/// Input: a recursive delete of `.`.
/// Expected: `FS_SANDBOX_DENIED` saying it is the workspace root, and the root
/// still there. It is a denial and not an I/O error because this build decided
/// it, not the kernel.
#[test]
fn the_workspace_root_itself_is_not_deletable() {
    let fixture = Fixture::new();
    fixture.write("kept.txt", "inside\n");
    let fs = fixture.sandboxed();

    let refused = fs
        .delete(&fs.resolve(".").expect("root"), true)
        .unwrap_err();

    assert_eq!(refused.code(), FsErrorCode::SandboxDenied);
    assert!(refused.to_string().contains("workspace root"), "{refused}");
    assert!(fixture.exists("kept.txt"));
}

/// TC-PORT-FS-28: composing the fenced backend with no fence is refused.
///
/// tetanus's own, and the reason [`backend`] exists.
///
/// Input: `SandboxedFs::new` asked for `danger-full-access`.
/// Expected: refused, saying the unfenced mode selects the other backend. A
/// confining type that accepts "confine nothing" grows a branch inside the
/// fence whose only job is to skip it - the one branch where a mistake is
/// silent.
#[test]
fn a_fenced_backend_refuses_to_be_composed_with_no_fence() {
    let fixture = Fixture::new();

    let composed = tetanus_fs::SandboxedFs::new(fixture.root(), FsMode::DangerFullAccess);
    let Err(refused) = composed else {
        panic!("a fenced backend must not compose itself with no fence")
    };

    assert_eq!(refused.code(), FsErrorCode::SandboxDenied);
    assert!(
        refused.to_string().contains("danger-full-access"),
        "{refused}"
    );
}
