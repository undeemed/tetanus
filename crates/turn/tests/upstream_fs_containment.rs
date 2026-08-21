//! Test Design Specification: filesystem path containment, ported.
//!
//! Feature under test: `tetanus_turn::fs::Workspace` - whether a path the
//! model chose resolves inside the workspace it was given. Upstream pins the
//! same rules in `packages/fs/fs-sandbox/tests/containment.spec.ts` and the
//! path half of its `fs-sandbox.spec.ts`; each case names the upstream case it
//! comes from.
//!
//! Approach: real directories, real symlinks, in a temp tree. A containment
//! rule asserted against a mocked filesystem would be asserting the mock:
//! every interesting case here - a link out, a link to a link, a new file
//! under a linked directory - is interesting precisely because of what the
//! kernel does with it, so the kernel is what answers.
//!
//! What is not restated, and why. Upstream's sandbox package also carries the
//! *mode* half - `read-only`, `workspace-write`, `danger-full-access`, and the
//! per-call escalation stamp - which is a policy layer over this one and has
//! no tetanus surface yet; `docs/parity.md` carries it. Its Windows 8.3 alias
//! and case-insensitivity cases are asserted through the same identity
//! fallback on the platform this runs on, since a case-insensitive volume is
//! not available here. Its TOCTOU case pins a direction rather than a
//! guarantee, and is restated as TC-PORT-FSC-9 with the same honesty about
//! what it does not prove.
//!
//! Environmental needs: a writable temp directory that supports symlinks, and
//! nothing else. No case reaches a network or an API key. Every case is
//! skipped on a platform without unix symlinks rather than silently passing.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs as unixfs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tetanus_turn::fs::{FsError, Workspace};

/// TC-PORT-FSC-1: the root itself, and anything under it, is inside.
///
/// Upstream: "accepts equal paths, descendants, and a filesystem-root
/// boundary", and "the workspace root itself passes the fence".
///
/// Input: the root named as `.`, a file in it, and a file nested two deep.
/// Expected: all three resolve, and each resolved path is the canonical one -
/// which is the value an operation must act on, since acting on the requested
/// spelling is what makes a check meaningless.
#[test]
fn the_root_and_its_descendants_are_inside() {
    let t = Tree::new();
    t.file("top.txt", "a");
    t.dir("nested/deeper");
    t.file("nested/deeper/leaf.txt", "b");
    let ws = t.workspace();

    assert_eq!(ws.resolve(".").expect("root").path, t.canonical_root());
    assert_eq!(
        ws.resolve("top.txt").expect("file").path,
        t.canonical_root().join("top.txt")
    );
    assert_eq!(
        ws.resolve("nested/deeper/leaf.txt").expect("nested").path,
        t.canonical_root().join("nested/deeper/leaf.txt")
    );
}

/// TC-PORT-FSC-2: an absolute path outside the workspace is refused.
///
/// Upstream: "an absolute path outside the workspace is denied, no file
/// created".
///
/// Naming an absolute path is not a way around the fence; it is a spelling
/// that is judged the same way as any other and usually fails.
///
/// Input: an absolute path to a sibling directory outside the root.
/// Expected: `Denied`, naming what was asked, where it resolved and the root
/// it was judged against - three facts, because a denial that only says "no"
/// is a denial nobody can act on.
#[test]
fn an_absolute_path_outside_the_workspace_is_refused() {
    let t = Tree::new();
    let outside = t.outside("secret.txt", "s");
    let ws = t.workspace();

    let refused = ws.resolve(&outside).expect_err("outside the workspace");

    match refused {
        FsError::Denied {
            requested,
            resolved,
            root,
        } => {
            assert_eq!(requested, outside.display().to_string());
            assert!(resolved.ends_with("secret.txt"), "{resolved}");
            assert_eq!(root, t.canonical_root().display().to_string());
        }
        other => panic!("expected a denial, got {other:?}"),
    }
}

/// TC-PORT-FSC-3: a `..` traversal out of the workspace is refused, and one
/// that comes back is not.
///
/// Upstream: "a `..` traversal out of the workspace is denied".
///
/// The second half is the one worth having: a rule that refused every `..`
/// would be easy and wrong, because `a/../b` never leaves. What is refused is
/// where a path *lands*, not how it is spelled.
///
/// Input: `../secret.txt`, then `nested/../top.txt`.
/// Expected: the first is denied; the second resolves to the file at the root.
#[test]
fn a_traversal_out_is_refused_and_one_that_returns_is_not() {
    let t = Tree::new();
    t.outside("secret.txt", "s");
    t.file("top.txt", "a");
    t.dir("nested");
    let ws = t.workspace();

    assert!(matches!(
        ws.resolve("../secret.txt"),
        Err(FsError::Denied { .. })
    ));
    assert_eq!(
        ws.resolve("nested/../top.txt").expect("returns").path,
        t.canonical_root().join("top.txt")
    );
}

/// TC-PORT-FSC-4: a symlinked directory inside the workspace that points out
/// is refused.
///
/// Upstream: "a symlinked directory inside the workspace pointing OUT is
/// denied (canonicalized before containment)".
///
/// This is the case that fails whenever containment is checked before
/// resolution: `workspace/link/secret.txt` is lexically under the workspace
/// and is actually anywhere the link goes.
///
/// Input: a link inside the root pointing at an outside directory, and a file
/// through it.
/// Expected: both the link and the path through it are denied, and the
/// resolved path in the denial is the outside location - so the report says
/// where the path really went.
#[test]
fn a_symlink_pointing_out_of_the_workspace_is_refused() {
    let t = Tree::new();
    let elsewhere = t.outside_dir("elsewhere");
    fs::write(elsewhere.join("secret.txt"), "s").expect("write");
    unixfs::symlink(&elsewhere, t.root().join("link")).expect("symlink");
    let ws = t.workspace();

    assert!(matches!(ws.resolve("link"), Err(FsError::Denied { .. })));

    match ws.resolve("link/secret.txt").expect_err("through the link") {
        FsError::Denied { resolved, .. } => assert!(
            resolved.ends_with("elsewhere/secret.txt"),
            "the denial says where it really went: {resolved}"
        ),
        other => panic!("expected a denial, got {other:?}"),
    }
}

/// TC-PORT-FSC-5: a file that does not exist yet is still judged, by the
/// directory it would be created in.
///
/// Upstream: "a NEW file created under a symlinked-out directory is denied
/// (deepest-ancestor realpath)".
///
/// This is the subtle one, and the reason resolution walks rather than calling
/// `canonicalize` once: a path that does not exist cannot be canonicalized at
/// all, so a rule that gave up on a missing target would let every *create*
/// through the fence - which is the operation most worth fencing.
///
/// Input: a new file under a linked-out directory, and a new file under a real
/// directory inside the workspace.
/// Expected: the first is denied on the strength of its parent, before
/// anything is created; the second resolves, reports `exists: false`, and
/// names where the create would land.
#[test]
fn a_file_that_does_not_exist_yet_is_judged_by_where_it_would_land() {
    let t = Tree::new();
    let elsewhere = t.outside_dir("elsewhere");
    unixfs::symlink(&elsewhere, t.root().join("link")).expect("symlink");
    t.dir("real");
    let ws = t.workspace();

    assert!(matches!(
        ws.resolve("link/new.txt"),
        Err(FsError::Denied { .. })
    ));
    assert!(
        !elsewhere.join("new.txt").exists(),
        "a denial creates nothing"
    );

    let allowed = ws.resolve("real/new.txt").expect("inside");
    assert!(!allowed.exists, "nothing is there yet");
    assert_eq!(allowed.path, t.canonical_root().join("real/new.txt"));
}

/// TC-PORT-FSC-6: a missing path is resolved component by component, so a link
/// met after a missing segment is still followed.
///
/// Upstream has no case for this: node's `resolve` normalizes the text first,
/// so the shape cannot arise there. It can here, and it is the exact mistake a
/// one-shot "normalize then canonicalize the parent" implementation makes:
/// `gone/../link` normalizes to `link`, and if the walk stops normalizing it
/// never notices that `link` exists and points out.
///
/// Input: a path through a directory that does not exist, back up, and into a
/// symlink that points out of the workspace.
/// Expected: denied, resolving to the outside target - the link was followed
/// because the walk reached it, not skipped because the text was tidied first.
#[test]
fn a_link_reached_after_a_missing_segment_is_still_followed() {
    let t = Tree::new();
    let elsewhere = t.outside_dir("elsewhere");
    unixfs::symlink(&elsewhere, t.root().join("link")).expect("symlink");
    let ws = t.workspace();

    match ws.resolve("gone/../link/secret.txt") {
        Err(FsError::Denied { resolved, .. }) => assert!(
            resolved.contains("elsewhere"),
            "the link was followed, not normalized away: {resolved}"
        ),
        other => panic!("expected a denial, got {other:?}"),
    }
}

/// TC-PORT-FSC-7: a sibling whose name merely starts with the root's is
/// outside.
///
/// Upstream compares with a separator-terminated prefix for this reason. It is
/// restated because the string form of the same check is a classic escape:
/// `/srv/workspace-old` starts with `/srv/workspace`.
///
/// Input: a workspace at `<tmp>/ws` and an absolute path into `<tmp>/ws-old`.
/// Expected: denied. Sharing a prefix is not being inside.
#[test]
fn a_sibling_sharing_the_roots_name_prefix_is_outside() {
    let t = Tree::new();
    let sibling = t.base().join("ws-old");
    fs::create_dir_all(&sibling).expect("mkdir");
    fs::write(sibling.join("secret.txt"), "s").expect("write");
    let ws = t.workspace();

    assert!(matches!(
        ws.resolve(sibling.join("secret.txt")),
        Err(FsError::Denied { .. })
    ));
}

/// TC-PORT-FSC-8: a workspace reached by another valid name still contains its
/// own paths.
///
/// Upstream: "recognizes an alias-equivalent root by filesystem identity for a
/// missing target", and the Windows 8.3/casing cases that motivate it. Neither
/// alias form exists on this platform, so the rule is exercised through the
/// alias this platform does have - a symlink to the root.
///
/// Without the identity fallback a deployment that named its workspace through
/// a link would refuse every path inside it, which reads as the fence being
/// broken rather than as the root being spelled differently.
///
/// Input: a workspace opened through a symlink to the real root, then a file
/// inside it, then a path outside.
/// Expected: the inside path resolves and the outside one is still denied -
/// the fallback widens recognition of the root, never the fence.
#[test]
fn a_root_reached_through_an_alias_still_contains_its_paths() {
    let t = Tree::new();
    t.file("top.txt", "a");
    let alias = t.base().join("alias");
    unixfs::symlink(t.root(), &alias).expect("symlink");

    let ws = Workspace::new(&alias).expect("workspace through an alias");

    assert!(ws.resolve("top.txt").is_ok(), "an aliased root still works");
    assert!(
        ws.resolve("missing.txt").is_ok(),
        "including for a path that is not there yet"
    );
    assert!(
        matches!(
            ws.resolve(t.outside("secret.txt", "s")),
            Err(FsError::Denied { .. })
        ),
        "and the fence is unchanged"
    );
}

/// TC-PORT-FSC-9: what this rule does not promise.
///
/// Upstream: "mutates the freshly checked identity, not a stale outside
/// targetKey (TOCTOU direction)". Upstream's case pins a direction, not a
/// guarantee, and so does this one.
///
/// A `Resolved` is a fact about the moment it was produced. An ancestor
/// swapped for a symlink afterwards is not caught, and cannot be at this
/// layer: closing that window needs the kernel boundary this module documents
/// itself as not being. What the design does is keep the window small, by
/// handing back the canonical path so a caller acts on what was judged rather
/// than re-resolving the requested spelling and acting on something else.
///
/// Input: a path resolved while its parent is a real directory, then the
/// parent replaced by a link out, then the same path resolved again.
/// Expected: the first answer is still the inside path it always was - a held
/// `Resolved` does not change under anyone - and the second call, which is the
/// one that re-checks, refuses. Re-resolving before acting is therefore the
/// caller's protection, and this case is what says so.
#[test]
fn a_resolved_path_is_a_fact_about_when_it_was_resolved() {
    let t = Tree::new();
    t.dir("swappable");
    let ws = t.workspace();

    let before = ws.resolve("swappable/file.txt").expect("inside");
    assert_eq!(before.path, t.canonical_root().join("swappable/file.txt"));

    let elsewhere = t.outside_dir("elsewhere");
    fs::remove_dir_all(t.root().join("swappable")).expect("remove");
    unixfs::symlink(&elsewhere, t.root().join("swappable")).expect("symlink");

    assert_eq!(
        before.path,
        t.canonical_root().join("swappable/file.txt"),
        "the value already handed out does not change"
    );
    assert!(
        matches!(
            ws.resolve("swappable/file.txt"),
            Err(FsError::Denied { .. })
        ),
        "and the next check sees the swap"
    );
}

/// TC-PORT-FSC-10: a root that is not there is refused, rather than becoming a
/// workspace that fences nothing.
///
/// Upstream: "denies unrelated and missing roots".
///
/// The direction is what matters: a fence that cannot find its post must
/// refuse, never fall through to allowing everything. Failing at construction
/// makes that structural instead of a rule every call has to remember.
///
/// Input: a root that does not exist, and a root that is a regular file.
/// Expected: `Root` in both cases, carrying the path and the underlying I/O
/// failure, so a deployment is told which of its two mistakes it made.
#[test]
fn a_root_that_cannot_be_resolved_is_refused_at_construction() {
    let t = Tree::new();

    let missing = Workspace::new(t.base().join("nope")).expect_err("no such root");
    assert!(matches!(missing, FsError::Root { .. }));

    let as_file = t.base().join("a-file");
    fs::write(&as_file, "x").expect("write");
    let refused = Workspace::new(&as_file).expect_err("a file is not a workspace");
    match refused {
        FsError::Root { root, .. } => assert_eq!(root, as_file.display().to_string()),
        other => panic!("expected a root failure, got {other:?}"),
    }
}

/// TC-PORT-FSC-11: a path under a regular file is missing, not contained
/// somewhere else.
///
/// Upstream: "treats a regular-file path segment as a missing target, not
/// containment".
///
/// Input: `top.txt/under.txt`, where `top.txt` is a file inside the workspace.
/// Expected: it resolves - it is under the root, and the walk keeps the
/// literal tail - and reports `exists: false`. The fence's job is where a path
/// is, not whether the operation that follows can succeed; letting the open
/// fail with the kernel's own `ENOTDIR` is a better error than one guessed
/// here.
#[test]
fn a_path_under_a_regular_file_is_missing_rather_than_uncontained() {
    let t = Tree::new();
    t.file("top.txt", "a");
    let ws = t.workspace();

    let resolved = ws.resolve("top.txt/under.txt").expect("still inside");
    assert!(!resolved.exists);
    assert_eq!(resolved.path, t.canonical_root().join("top.txt/under.txt"));
}

/// TC-PORT-FSC-12: a chain of links inside the workspace is followed to its
/// end, and judged there.
///
/// Upstream: covered implicitly by canonicalization; stated here because "one
/// link deep" is the easy case to get right and the wrong place to stop.
///
/// Input: a link to a link to a directory outside, and separately a link to a
/// link to a file inside.
/// Expected: the first is denied and the second resolves to the real inside
/// file - the end of the chain decides, not the first hop.
#[test]
fn a_chain_of_links_is_judged_at_its_end() {
    let t = Tree::new();
    let elsewhere = t.outside_dir("elsewhere");
    unixfs::symlink(&elsewhere, t.root().join("hop1")).expect("symlink");
    unixfs::symlink(t.root().join("hop1"), t.root().join("hop2")).expect("symlink");

    t.file("real.txt", "a");
    unixfs::symlink(t.root().join("real.txt"), t.root().join("in1")).expect("symlink");
    unixfs::symlink(t.root().join("in1"), t.root().join("in2")).expect("symlink");

    let ws = t.workspace();

    assert!(matches!(ws.resolve("hop2"), Err(FsError::Denied { .. })));
    assert_eq!(
        ws.resolve("in2").expect("inside").path,
        t.canonical_root().join("real.txt")
    );
}

// ---------------------------------------------------------------- fixtures

/// A temp tree with a workspace root and room beside it for the outside.
///
/// The root is a *subdirectory* of the temp dir on purpose, so "outside the
/// workspace but still inside the temp dir" is expressible - which is what
/// every denial case needs, and what keeps the suite from writing anywhere a
/// real system would notice.
struct Tree {
    dir: TempDir,
}

impl Tree {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        fs::create_dir_all(dir.path().join("ws")).expect("mkdir");
        Self { dir }
    }

    fn base(&self) -> &Path {
        self.dir.path()
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("ws")
    }

    /// The root as the workspace reports it. macOS puts temp dirs under a
    /// symlinked `/var`, so the requested root and its canonical form differ
    /// there; comparing against this rather than against `root()` is what
    /// keeps the suite honest on both platforms.
    fn canonical_root(&self) -> PathBuf {
        fs::canonicalize(self.root()).expect("canonical root")
    }

    fn workspace(&self) -> Workspace {
        Workspace::new(self.root()).expect("workspace")
    }

    fn dir(&self, relative: &str) {
        fs::create_dir_all(self.root().join(relative)).expect("mkdir");
    }

    fn file(&self, relative: &str, content: &str) {
        let path = self.root().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, content).expect("write");
    }

    /// A file beside the workspace, not in it.
    fn outside(&self, name: &str, content: &str) -> PathBuf {
        let path = self.base().join(name);
        fs::write(&path, content).expect("write");
        path
    }

    /// A directory beside the workspace, not in it.
    fn outside_dir(&self, name: &str) -> PathBuf {
        let path = self.base().join(name);
        fs::create_dir_all(&path).expect("mkdir");
        fs::canonicalize(path).expect("canonical")
    }
}
