//! Test Design Specification: one policy, enforced on the filesystem side by
//! the kernel.
//!
//! Feature under test: `tetanus_fs::kernel::KernelConfined` - the same
//! [`tetanus_sandbox::Policy`] the shell tools run behind, applied to the file
//! service. Upstream keeps the two apart (`packages/fs/fs-sandbox` fences
//! paths, `packages/sandbox/sandbox-local` confines its bash runner) and its
//! `sandbox/src/roots.ts` names the hazard that creates: "the write tool
//! cannot write /tmp but bash can" is what happens when two layers derive the
//! same permission separately.
//!
//! Approach: **the fence is deliberately set wider than the policy**, so our
//! own check says yes and only the kernel can say no. A case that fenced and
//! confined the same directory would pass against a build with no kernel
//! layer at all, which is the one result that would prove nothing. Every
//! denial here is therefore attributed: the same operation is run through the
//! same backend without the kernel layer and succeeds.
//!
//! What is not restated, and why. This is the follow-up
//! `docs/parity.md` named, so the policy
//! vocabulary, the backend probe and the degraded-kernel refusal are already
//! ported in `crates/sandbox/tests/upstream_sandbox.rs` (TC-PORT-SANDBOX-1..12)
//! and the process side in `crates/exec/tests/upstream_sandbox_exec.rs`
//! (-13..-19). What is added here is the filesystem half and the claim that
//! binds the two. The approved-escalation retry is still a follow-up: it needs
//! the approval seam, not another enforcement layer.
//!
//! Environmental needs: Linux with Landlock, a writable temp directory. Cases
//! report themselves skipped on a kernel without it rather than passing for
//! the wrong reason. No case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use tetanus_fs::access::FsMode;
use tetanus_fs::error::FsErrorCode;
use tetanus_fs::kernel::KernelConfined;
use tetanus_fs::service::{FileSystem, WriteIntent};
use tetanus_fs::{SandboxedFs, WriteOperation};
use tetanus_sandbox::{Enforcement, Mode, Policy};

/// TC-PORT-SANDBOX-20: our own fence allows the path and the kernel refuses
/// it.
///
/// This is the case the whole slice exists for. The fence is `workspace-write`
/// over the workspace, so `SandboxedFs` resolves the path and permits the
/// mutation - this build's own check says yes. The policy behind it is
/// `read-only`, so Landlock says no. Nothing in tetanus cooperates in the
/// refusal.
///
/// The attribution is the second half: the identical write through the
/// identical backend, without the kernel layer, succeeds. Without that, a
/// build whose fence had quietly refused would pass this case.
///
/// Input: one workspace; a `SandboxedFs` in `workspace-write` over it; the
/// same backend wrapped under a `read-only` policy; a write to a path inside
/// the workspace through each.
/// Expected: unwrapped, the write lands. Wrapped, it is refused with
/// `FS_PERMISSION_DENIED` - the class that means the operating system refused,
/// not `FS_SANDBOX_DENIED`, which is the class that means this build decided -
/// and the file is not created.
#[test]
fn the_fence_allows_it_and_the_kernel_refuses_it() {
    let Some(workspace) = enforcing() else { return };
    let unconfined = fenced(workspace.path());

    // Our own check: this path is inside the workspace and the mode permits
    // writing, so the fence lets it through.
    let target = unconfined
        .resolve("allowed-by-the-fence.txt")
        .expect("resolved");
    unconfined
        .write(
            &target,
            "the fence permits this",
            &WriteIntent::CreateIfAbsent,
        )
        .expect("the fence and the mode both allow it");
    std::fs::remove_file(target.path()).expect("tidied for the confined attempt");

    let confined = KernelConfined::new(
        Arc::new(fenced(workspace.path())),
        Policy::new(Mode::ReadOnly, workspace.path()),
    )
    .expect("this host can enforce it");
    assert_eq!(confined.enforcement(), Enforcement::Full);

    let target = confined
        .resolve("allowed-by-the-fence.txt")
        .expect("resolved");
    let refused = confined
        .write(&target, "the kernel does not", &WriteIntent::CreateIfAbsent)
        .expect_err("the kernel allowed a write the policy forbids");

    assert_eq!(
        refused.code(),
        FsErrorCode::PermissionDenied,
        "a kernel denial is the operating system refusing, not this build deciding: {refused}"
    );
    assert!(
        !target.path().exists(),
        "the refused write created {}",
        target.path().display()
    );
}

/// TC-PORT-SANDBOX-21: reading is untouched by a confining policy.
///
/// The complement of -20, and the case that would catch the most likely way to
/// break this: a worker that restricted itself too narrowly would make the
/// file service useless while looking, from the outside, like a policy working
/// as intended.
///
/// Input: a file written before confinement, read back through the confined
/// service, plus a stat and a listing.
/// Expected: all three succeed and answer what is really there.
#[test]
fn reading_is_untouched_by_a_confining_policy() {
    let Some(workspace) = enforcing() else { return };
    std::fs::write(workspace.path().join("readable.txt"), "contents").expect("written");

    let confined = KernelConfined::new(
        Arc::new(fenced(workspace.path())),
        Policy::new(Mode::ReadOnly, workspace.path()),
    )
    .expect("this host can enforce it");

    let target = confined.resolve("readable.txt").expect("resolved");
    let (text, _version) = confined.read(&target).expect("reads are permitted");
    assert_eq!(text, "contents");
    assert!(confined.stat(&target).expect("stat").is_some());

    let root = confined.resolve(".").expect("resolved");
    let listing = confined.list(&root).expect("listing is permitted");
    assert!(listing.iter().any(|entry| entry.name == "readable.txt"));
}

/// TC-PORT-SANDBOX-22: confining the file service does not confine the
/// harness.
///
/// Landlock is per thread and cannot be undone, so the arrangement this module
/// chose - one worker that restricts itself - is load-bearing. If the
/// restriction had been applied to the process's own threads instead, the
/// journal, the settings document and the socket would all have stopped
/// working, and the failure would look like everything breaking at once
/// rather than like a sandbox.
///
/// Input: a confined service, then ordinary writes from the test's own thread
/// to the workspace and to a path outside every granted root.
/// Expected: both succeed. The boundary is the worker's, not the process's.
#[test]
fn confining_the_file_service_does_not_confine_the_harness() {
    let Some(workspace) = enforcing() else { return };
    let _confined = KernelConfined::new(
        Arc::new(fenced(workspace.path())),
        Policy::new(Mode::ReadOnly, workspace.path()),
    )
    .expect("this host can enforce it");

    let inside = workspace.path().join("written-by-the-harness.txt");
    std::fs::write(&inside, "the harness still works").expect("the harness is not confined");
    assert_eq!(
        std::fs::read_to_string(&inside).expect("read back"),
        "the harness still works"
    );

    // A journal lives outside the model's workspace, which is the case that
    // would break first if this were wrong.
    let elsewhere = tempfile::tempdir().expect("temp dir");
    std::fs::write(elsewhere.path().join("journal.jsonl"), "{}\n")
        .expect("the harness can still write its own files");
}

/// TC-PORT-SANDBOX-23: one policy, both seams, the same answer.
///
/// The requirement the sandbox lane's parity note named: "expressed once and
/// applied by both the filesystem service and the process executor". Upstream
/// derives its writable roots in one module for this reason, and states the
/// asymmetry it prevents - "the write tool cannot write /tmp but bash can".
///
/// Input: one `Policy` value; a write to the same path attempted through the
/// confined file service and through a confined shell command.
/// Expected: both are refused, and the file exists after neither.
#[tokio::test]
async fn one_policy_gives_both_seams_the_same_answer() {
    let Some(workspace) = enforcing() else { return };
    // One value, cloned into each seam - not two policies that happen to agree.
    let policy = Policy::new(Mode::ReadOnly, workspace.path());
    let contested = workspace.path().join("contested.txt");

    let files = KernelConfined::new(Arc::new(fenced(workspace.path())), policy.clone())
        .expect("this host can enforce it");
    let target = files.resolve("contested.txt").expect("resolved");
    let by_the_file_tool = files.write(&target, "from the file tool", &WriteIntent::CreateIfAbsent);

    let shell = tetanus_exec::shell::ShellExec::new(
        Arc::new(tetanus_exec::backend::Bash::new()),
        tetanus_exec::shell::ShellConfig {
            cwd: workspace.path().to_path_buf(),
            grace: std::time::Duration::from_millis(200),
            sandbox: policy,
            ..tetanus_exec::shell::ShellConfig::default()
        },
    )
    .expect("this host can enforce it");
    let spec = shell
        .resolve(tetanus_exec::shell::ShellRequest::new(format!(
            "echo from the shell > {}",
            contested.display()
        )))
        .expect("resolved");
    let by_the_shell = shell.run(&spec).await.expect("the shell ran");

    assert_eq!(
        by_the_file_tool
            .expect_err("the file tool was allowed to write")
            .code(),
        FsErrorCode::PermissionDenied
    );
    assert!(
        !by_the_shell.output.ok(),
        "the shell was allowed to write: {}",
        tetanus_exec::shell::render(&by_the_shell)
    );
    assert!(
        !contested.exists(),
        "one of the two seams wrote the file the policy forbids"
    );
}

/// TC-PORT-SANDBOX-24: a policy a host cannot enforce composes no service.
///
/// The same rule the executor follows, at the same moment: a service that
/// answered calls while enforcing nothing is the failure the whole crate
/// exists to prevent, and the place to refuse is where a deployment is still
/// being built.
///
/// Input: on a kernel that cannot govern the whole policy, a service asked for
/// it; on a capable kernel, the same policy reporting full enforcement.
/// Expected: refusal naming what is missing, or `Full`.
#[test]
fn a_policy_this_host_cannot_enforce_composes_no_service() {
    let Some(workspace) = enforcing() else { return };
    let support = tetanus_sandbox::support().expect("landlock is here");
    let strict =
        Policy::new(Mode::WorkspaceWrite, workspace.path()).network(tetanus_sandbox::Network::Deny);

    let composed = KernelConfined::new(Arc::new(fenced(workspace.path())), strict);

    if support.governs_network && support.governs_truncate {
        assert_eq!(
            composed
                .expect("a capable kernel enforces it")
                .enforcement(),
            Enforcement::Full
        );
    } else {
        assert!(
            composed.is_err(),
            "an under-capable host must not compose a file service that claims to confine"
        );
    }
}

/// TC-PORT-SANDBOX-25: every mutation is governed, not only `write`.
///
/// Landlock's handled set is every right the kernel knows, so removing and
/// replacing are governed too. Asserting only `write` would leave the most
/// destructive operation - delete - unproven, and a policy that stopped writes
/// while permitting deletes would be worse than none.
///
/// Input: an existing file inside the fence, and a confined service asked to
/// edit it and to delete it.
/// Expected: both refused by the operating system, and the file is unchanged
/// afterwards.
#[test]
fn every_mutation_is_governed_not_only_write() {
    let Some(workspace) = enforcing() else { return };
    let victim = workspace.path().join("victim.txt");
    std::fs::write(&victim, "original").expect("written before confinement");

    let confined = KernelConfined::new(
        Arc::new(fenced(workspace.path())),
        Policy::new(Mode::ReadOnly, workspace.path()),
    )
    .expect("this host can enforce it");
    let target = confined.resolve("victim.txt").expect("resolved");

    let edited = confined.edit(
        &target,
        &tetanus_fs::service::EditRequest {
            old: "original".to_string(),
            new: "rewritten".to_string(),
            replace_all: false,
        },
        None,
    );
    let deleted = confined.delete(&target, false);

    for (what, code) in [
        ("edit", edited.err().map(|error| error.code())),
        ("delete", deleted.err().map(|error| error.code())),
    ] {
        assert_eq!(
            code,
            Some(FsErrorCode::PermissionDenied),
            "{what} was not refused by the operating system"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&victim).expect("still there"),
        "original",
        "a refused mutation changed the file anyway"
    );
}

/// TC-PORT-SANDBOX-26: a write the policy does permit still works.
///
/// The boundary has to be usable, or a deployment will turn it off. Under
/// `workspace-write` the file tools do their job, and this is the case that
/// keeps the previous five from being satisfied by a service that refuses
/// everything.
///
/// Input: a `workspace-write` policy over the workspace the fence covers, and
/// a write, an edit and a delete inside it.
/// Expected: all three succeed, and the file system shows it.
#[test]
fn work_the_policy_permits_still_works() {
    let Some(workspace) = enforcing() else { return };
    let confined = KernelConfined::new(
        Arc::new(fenced(workspace.path())),
        Policy::new(Mode::WorkspaceWrite, workspace.path()),
    )
    .expect("this host can enforce it");

    let target = confined.resolve("work.txt").expect("resolved");
    let written = confined
        .write(&target, "first", &WriteIntent::CreateIfAbsent)
        .expect("workspace-write permits a write");
    assert_eq!(written.operation, WriteOperation::Create);

    confined
        .edit(
            &target,
            &tetanus_fs::service::EditRequest {
                old: "first".to_string(),
                new: "second".to_string(),
                replace_all: false,
            },
            None,
        )
        .expect("and an edit");
    assert_eq!(
        std::fs::read_to_string(target.path()).expect("read back"),
        "second"
    );

    confined.delete(&target, false).expect("and a delete");
    assert!(!target.path().exists());
}

// ---------------------------------------------------------------- fixtures

/// A workspace to fence, or `None` after reporting the case skipped on a
/// kernel with no Landlock.
fn enforcing() -> Option<tempfile::TempDir> {
    match tetanus_sandbox::support() {
        Ok(_) => Some(tempfile::tempdir().expect("temp dir")),
        Err(why) => {
            eprintln!("skipped: {why}");
            None
        }
    }
}

/// The fenced backend, deliberately wider than the policy under test: this
/// build's own check permits the mutation, so only the kernel can refuse it.
fn fenced(root: &std::path::Path) -> SandboxedFs {
    SandboxedFs::new(root, FsMode::WorkspaceWrite).expect("a workspace to fence")
}

/// TC-PORT-SANDBOX-27: one call composes the mode and the policy together.
///
/// The same argument [`tetanus_fs::access::backend`] makes for the mode and the
/// backend, extended by one layer: a deployment that has to remember to wrap
/// its filesystem will one day forget, and the forgetting is silent.
///
/// Input: `confined_backend` with a confining policy, and with
/// `danger-full-access`.
/// Expected: the first is the kernel-confined backend and refuses a write the
/// policy forbids; the second is the plain backend, which is what a deployment
/// that asked for no boundary should get rather than a worker thread it does
/// not need.
#[test]
fn one_call_composes_the_mode_and_the_policy() {
    let Some(workspace) = enforcing() else { return };

    let confined = tetanus_fs::kernel::confined_backend(
        FsMode::WorkspaceWrite,
        workspace.path(),
        &Policy::new(Mode::ReadOnly, workspace.path()),
    )
    .expect("this host can enforce it");
    assert_eq!(confined.backend(), "kernel-confined");
    let target = confined.resolve("blocked.txt").expect("resolved");
    assert_eq!(
        confined
            .write(&target, "no", &WriteIntent::CreateIfAbsent)
            .expect_err("the policy forbids it")
            .code(),
        FsErrorCode::PermissionDenied
    );

    let plain = tetanus_fs::kernel::confined_backend(
        FsMode::WorkspaceWrite,
        workspace.path(),
        &Policy::danger_full_access(workspace.path()),
    )
    .expect("an unconfined policy composes anywhere");
    assert_ne!(
        plain.backend(),
        "kernel-confined",
        "a deployment that asked for no boundary should not pay for a worker thread"
    );
    let target = plain.resolve("allowed.txt").expect("resolved");
    plain
        .write(&target, "yes", &WriteIntent::CreateIfAbsent)
        .expect("no policy, no refusal");
}
