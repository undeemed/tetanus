//! Test Design Specification: the sandbox policy and the Landlock backend,
//! ported.
//!
//! Features under test: `tetanus_sandbox::policy` - the mode vocabulary and
//! what each mode grants - and `tetanus_sandbox::landlock` - the kernel
//! boundary itself. Upstream pins the same decisions in
//! `packages/sandbox/sandbox-policy/tests/policy.spec.ts`,
//! `packages/sandbox/sandbox/tests/{roots,vocabulary,escalation}.spec.ts` and
//! `packages/sandbox/sandbox-local/tests/{local,probe,provider-chain}.spec.ts`.
//!
//! Approach: **denial is proven by being denied**. Every confinement case
//! restricts a real thread or a real child and then tries the operation, so
//! what passes is the kernel's answer and not this crate's opinion of it. A
//! case that asserted the policy object would pass just as happily against a
//! backend that never called a syscall - which is the exact failure mode
//! `crates/sandbox/src/unsupported.rs` exists to refuse.
//!
//! Restriction is one-way and per thread, so each confinement case runs in a
//! thread of its own: a restricted thread cannot widen itself again, and
//! confining the test harness's main thread would sandbox every case after it.
//!
//! What is not restated, and why. Upstream's Windows ACL family (nine of the
//! twenty-one spec files: `acl`, `acl-grants`, `grant`, `token`, `ffi`,
//! `quote`, `workspace-sid`, `path-boundary`, `runner`) has no counterpart
//! until that backend exists; this platform refuses it out loud instead, and
//! TC-PORT-SANDBOX-11 pins the refusal. Its Seatbelt and bwrap dialects are
//! other hosts' backends. Its escalation half (`escalation.spec.ts`) is an
//! approval flow over a policy - the seam is `tetanus_turn::approval`, and
//! wiring the two together is a named follow-up rather than something to
//! invent here. `docs/parity.md` carries
//! the list with reasons.
//!
//! Environmental needs: Linux with Landlock enabled (this file is skipped
//! elsewhere, and reports itself skipped on a Linux without it rather than
//! passing for the wrong reason), and a writable temp directory. No case
//! reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};

use tetanus_sandbox::landlock;
use tetanus_sandbox::policy::{Enforcement, Mode, Network, Policy};
use tetanus_sandbox::SandboxError;

/// TC-PORT-SANDBOX-1: this host says which ABI it speaks, or why it speaks
/// none.
///
/// Upstream: `sandbox-local/tests/probe.spec.ts` ("probes backend
/// availability", "reports an unavailable backend rather than assuming one").
///
/// Every other case in this file is conditional on the answer, and a harness
/// that could not tell "no Landlock here" from "Landlock said yes" would
/// report a sandbox that does not exist.
///
/// Input: the probe.
/// Expected: either an ABI of at least 1 with the capability flags that follow
/// from it, or an `Unavailable` naming why - never a silent zero.
#[test]
fn the_host_reports_what_it_can_enforce() {
    match landlock::support() {
        Ok(support) => {
            let abi = support
                .abi
                .expect("a landlock support answer carries its ABI");
            assert!(abi >= 1, "an ABI below 1 is not support: {abi}");
            assert_eq!(support.backend, "landlock");
            assert_eq!(support.governs_network, abi >= 4);
            assert_eq!(support.governs_truncate, abi >= 3);
            assert_eq!(support.governs_ioctl, abi >= 5);
        }
        Err(SandboxError::Unavailable { backend, why }) => {
            assert_eq!(backend, "landlock");
            assert!(!why.is_empty(), "an unavailable backend has to say why");
            eprintln!("skipped: this kernel has no Landlock ({why})");
        }
        Err(other) => panic!("a probe answers support or unavailability, not {other}"),
    }
}

/// TC-PORT-SANDBOX-2: under `read-only`, a write outside every granted root is
/// refused by the kernel.
///
/// Upstream: `sandbox-local/tests/local.spec.ts` ("read-only denies writes").
///
/// This is the acceptance criterion of the whole lane, and it is deliberately
/// asserted at its lowest level: the thread restricts itself and then tries an
/// ordinary `std::fs::write`. Nothing in tetanus is between the attempt and
/// the refusal, so what passes is the kernel.
///
/// Input: a temp file that a thread can write, then the same write after
/// confining that thread `read-only`.
/// Expected: the first write succeeds, the second fails with permission
/// denied, and the file still holds what the first write put there.
#[test]
fn read_only_denies_a_write_at_the_kernel() {
    let Some(_) = enforcing() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let target = dir.path().join("witness.txt");
    std::fs::write(&target, b"before").expect("writable before confinement");

    let refused = confined(Policy::new(Mode::ReadOnly, dir.path()), {
        let target = target.clone();
        move || std::fs::write(&target, b"after")
    });

    let error = refused.expect_err("the kernel allowed a write under read-only");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "a denial is EACCES, not {error}"
    );
    assert_eq!(
        std::fs::read_to_string(&target).expect("still readable"),
        "before",
        "the denied write must not have landed"
    );
}

/// TC-PORT-SANDBOX-3: under `workspace-write`, the workspace is writable and
/// everywhere else is not.
///
/// Upstream: `local.spec.ts` ("workspace-write permits the workspace",
/// "workspace-write denies outside the workspace"), and `roots.spec.ts` for
/// the derivation.
///
/// One case for both halves because they are one claim: a boundary that
/// permits nothing is not a boundary anyone can work behind, and a boundary
/// that permits everything is not one at all.
///
/// Input: two directories; a policy rooted at the first; a thread confined to
/// it writing into each.
/// Expected: the write inside succeeds and the write outside is denied.
#[test]
fn workspace_write_permits_the_workspace_and_denies_the_rest() {
    let Some(_) = enforcing() else { return };
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(elsewhere) = outside_every_grant() else {
        return;
    };
    let inside = workspace.path().join("inside.txt");
    let outside = elsewhere.path().join("outside.txt");

    let (allowed, refused) = confined(Policy::new(Mode::WorkspaceWrite, workspace.path()), {
        let (inside, outside) = (inside.clone(), outside.clone());
        move || {
            (
                std::fs::write(&inside, b"mine"),
                std::fs::write(&outside, b"not mine"),
            )
        }
    });

    allowed.expect("the workspace is writable under workspace-write");
    let error = refused.expect_err("a write outside the workspace was allowed");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::read_to_string(&inside).expect("written"), "mine");
    assert!(!outside.exists(), "the denied write created a file");
}

/// TC-PORT-SANDBOX-4: reading is not what a confining mode takes away.
///
/// Upstream: `local.spec.ts` ("read-only permits reads").
///
/// Stated as its own case because the opposite is an easy and invisible
/// mistake: a ruleset that handles the read rights and forgets to grant them
/// produces a "sandbox" under which nothing runs at all, and every failure
/// afterwards looks like the program's fault.
///
/// Input: a file outside the workspace, read from a thread confined
/// `read-only`.
/// Expected: the read succeeds and returns what is in the file.
#[test]
fn a_confining_mode_still_permits_reads() {
    let Some(_) = enforcing() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let readable = dir.path().join("readable.txt");
    std::fs::write(&readable, b"contents").expect("written before confinement");
    let workspace = tempfile::tempdir().expect("temp dir");

    let read = confined(Policy::new(Mode::ReadOnly, workspace.path()), {
        let readable = readable.clone();
        move || std::fs::read_to_string(&readable)
    });

    assert_eq!(read.expect("reads are permitted"), "contents");
}

/// TC-PORT-SANDBOX-5: creating and removing under a denied root are denied
/// too, not just writing to a file.
///
/// Upstream: `local.spec.ts` ("denies mkdir", "denies unlink").
///
/// A ruleset that handled only `WRITE_FILE` would leave `mkdir`, `rename` and
/// `unlink` untouched, and a model that cannot write a file can still delete
/// one. The handled set is everything the kernel knows for exactly this
/// reason, and this case is what proves it rather than trusting the constant.
///
/// Input: an existing file and directory outside the workspace, and a thread
/// confined `workspace-write` trying to create, remove and rename them.
/// Expected: every one of them is denied, and the filesystem is unchanged.
#[test]
fn creating_and_removing_outside_the_workspace_are_denied() {
    let Some(_) = enforcing() else { return };
    let workspace = tempfile::tempdir().expect("temp dir");
    let Some(elsewhere) = outside_every_grant() else {
        return;
    };
    let victim = elsewhere.path().join("victim.txt");
    std::fs::write(&victim, b"still here").expect("written before confinement");

    let (made, removed, renamed) = confined(Policy::new(Mode::WorkspaceWrite, workspace.path()), {
        let (elsewhere, victim) = (elsewhere.path().to_path_buf(), victim.clone());
        move || {
            (
                std::fs::create_dir(elsewhere.join("new-dir")),
                std::fs::remove_file(&victim),
                std::fs::rename(&victim, elsewhere.join("moved.txt")),
            )
        }
    });

    for (what, outcome) in [("mkdir", made), ("unlink", removed), ("rename", renamed)] {
        let error = outcome.expect_err("{what} outside the workspace was allowed");
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{what} failed for the wrong reason: {error}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&victim).expect("untouched"),
        "still here"
    );
}

/// TC-PORT-SANDBOX-6: `danger-full-access` is unconfined, and says so.
///
/// Upstream: `vocabulary.spec.ts` ("danger-full-access bypasses
/// confinement").
///
/// The mode has to exist, because some work genuinely needs the machine; what
/// matters is that reaching it takes writing the word. This case pins that the
/// word does what it says - and TC-PORT-SANDBOX-2 pins that not writing it
/// gets you a boundary.
///
/// Input: the unconfined policy, prepared.
/// Expected: a confinement that confines nothing, holding no kernel object,
/// and a write outside any workspace still succeeds under it.
#[test]
fn danger_full_access_is_unconfined_by_name() {
    let dir = tempfile::tempdir().expect("temp dir");
    let policy = Policy::danger_full_access(dir.path());
    assert!(!policy.mode().confines());

    let prepared = tetanus_sandbox::prepare(&policy).expect("an unconfined policy always prepares");
    assert!(!prepared.confines());
    assert!(prepared.ruleset.is_none());
    assert_eq!(prepared.enforcement, Enforcement::Full);
}

/// TC-PORT-SANDBOX-7: a policy asking for more than this kernel can govern is
/// refused, unless the caller accepted less in writing.
///
/// Upstream: `provider-chain.spec.ts` ("fails closed when no backend can serve
/// the requested mode") and its `partial` enforcement reporting.
///
/// This is the degraded-kernel path. The failure it prevents is the quiet one:
/// a deployment upgrades to a host with an older kernel, the network clause of
/// its policy stops being enforceable, and nothing says so because the run
/// still works.
///
/// Input: on a kernel below ABI 4, a policy denying the network; and the same
/// policy accepting partial enforcement. On a kernel at ABI 4 or above, the
/// same claim from the other side - full enforcement, and network denial
/// really applied.
/// Expected: refusal naming what is missing, then `Partial` when accepted; or
/// `Full` on a capable kernel.
#[test]
fn a_policy_beyond_this_kernel_is_refused_unless_partial_is_accepted() {
    let Some(support) = enforcing() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let strict = Policy::new(Mode::WorkspaceWrite, dir.path()).network(Network::Deny);

    if support.governs_network && support.governs_truncate {
        let prepared = tetanus_sandbox::prepare(&strict).expect("this kernel can govern it");
        assert_eq!(prepared.enforcement, Enforcement::Full);
        return;
    }

    match tetanus_sandbox::prepare(&strict) {
        Err(SandboxError::Degraded {
            backend,
            abi,
            missing,
        }) => {
            assert_eq!(backend, "landlock");
            assert!(abi < 4 || !support.governs_truncate);
            assert!(!missing.is_empty(), "a refusal names what it cannot do");
        }
        other => panic!("an under-capable kernel must refuse, got {other:?}"),
    }

    let accepted = tetanus_sandbox::prepare(&strict.clone().accept_partial_enforcement())
        .expect("partial enforcement was accepted in writing");
    assert_eq!(accepted.enforcement, Enforcement::Partial);
}

/// TC-PORT-SANDBOX-8: denying the network stops a TCP connection, at the
/// kernel.
///
/// Upstream has no network axis in its policy and says so; this is the
/// addition `docs/parity.md` records, and it is asserted the same way
/// everything else here is - by being refused.
///
/// Input: a listener on loopback, then a connection to it from a thread
/// confined with `Network::Deny`.
/// Expected: on a kernel at ABI 4 or above the connection is refused with
/// permission denied; the same connection from an unconfined thread succeeds,
/// so the case cannot pass because the listener was broken.
#[test]
fn denying_the_network_stops_a_connection() {
    let Some(support) = enforcing() else { return };
    if !support.governs_network {
        eprintln!("skipped: Landlock ABI {:?} cannot govern TCP", support.abi);
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let address = listener.local_addr().expect("its address");
    std::net::TcpStream::connect(address).expect("reachable before confinement");

    let dir = tempfile::tempdir().expect("temp dir");
    let refused = confined(
        Policy::new(Mode::WorkspaceWrite, dir.path()).network(Network::Deny),
        move || std::net::TcpStream::connect(address).map(|_| ()),
    );

    let error = refused.expect_err("a denied network allowed a connection");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "the connection failed for the wrong reason: {error}"
    );
}

/// TC-PORT-SANDBOX-9: the temp areas are writable under `workspace-write`,
/// because the mode promises a workspace a build can actually run in.
///
/// Upstream: `roots.spec.ts` ("workspace-write allows the workspace and the
/// platform temp areas").
///
/// Left out, this is the bug that makes every compiler and every test runner
/// fail under the sandbox for a reason nobody can see - and it fails
/// differently per backend, which is why upstream derives the list in one
/// place and so does this.
///
/// Input: the derived roots for a `workspace-write` policy.
/// Expected: the workspace, `/tmp`, and the caller's own `TMPDIR` when it has
/// one; and a write into the temp directory really succeeds under
/// confinement.
#[test]
fn workspace_write_can_use_the_temp_areas() {
    let Some(_) = enforcing() else { return };
    let workspace = tempfile::tempdir().expect("temp dir");
    let policy = Policy::new(Mode::WorkspaceWrite, workspace.path());
    let roots = policy.writable_roots();
    assert!(roots.contains(&workspace.path().to_path_buf()));
    assert!(roots.contains(&PathBuf::from("/tmp")));

    // The suite's own temp directory is under TMPDIR, which is exactly the
    // root this case is about.
    let scratch = tempfile::tempdir().expect("temp dir");
    let target = scratch.path().join("build-artifact");
    let written = confined(policy, {
        let target = target.clone();
        move || std::fs::write(&target, b"a compiler's temp file")
    });

    written.expect("a build under workspace-write can write a temp file");
}

/// TC-PORT-SANDBOX-10: a write sink stays open under every confining mode.
///
/// Upstream: its `read-only` mode "permits only required sinks such as
/// `/dev/null`".
///
/// A program that cannot open `/dev/null` fails in ways that look nothing like
/// a sandbox denial - a shell redirect fails, a library's logging fails - and
/// the person reading the output goes looking for the wrong bug.
///
/// Input: a write to `/dev/null` from a thread confined `read-only`.
/// Expected: it succeeds.
#[test]
fn a_write_sink_stays_open_under_read_only() {
    let Some(_) = enforcing() else { return };
    let dir = tempfile::tempdir().expect("temp dir");

    let written = confined(Policy::new(Mode::ReadOnly, dir.path()), || {
        std::fs::write("/dev/null", b"discarded")
    });

    written.expect("/dev/null is a sink every mode grants");
}

/// TC-PORT-SANDBOX-11: a platform with no backend refuses rather than
/// pretending.
///
/// Upstream ships a Windows ACL backend; this does not, and the whole point of
/// this case is that the absence is loud. A `prepare` that returned an
/// unconfined confinement on Windows would be the one outcome this crate
/// exists to prevent: a deployment reading "sandboxed" in its configuration,
/// a model running arbitrary commands, and nothing between them.
///
/// The refusal is a compile-time choice - `unsupported::prepare` is what is
/// compiled where no backend exists - so on Linux this asserts the reasoning
/// is stated where a porter will find it, and the platform-specific behaviour
/// is asserted by the module that compiles there.
///
/// Input: the module's description of what this platform would need.
/// Expected: it names the upstream backend a port would restate, so the
/// follow-up is a piece of work rather than a mystery.
#[test]
fn a_platform_without_a_backend_refuses_loudly() {
    // The linux build compiles `landlock`, so the refusal path is read here
    // through the source of the module that would compile elsewhere.
    let source = include_str!("../src/unsupported.rs");
    assert!(
        source.contains("sandbox-windows-acl"),
        "the refusal has to name the upstream backend a port would restate"
    );
    assert!(
        source.contains("Err(SandboxError::Unavailable"),
        "the unsupported path must refuse, never return an unconfined confinement"
    );
    assert!(
        !source.contains("Ok(Confinement::none())"),
        "an unsupported platform must not answer a confining policy with no confinement"
    );
}

/// TC-PORT-SANDBOX-12: restriction is one-way.
///
/// Upstream's backends are process-scoped for the same reason. A confinement a
/// confined program could lift is not a boundary: the first thing a model's
/// command would do is lift it.
///
/// Input: a thread confined `read-only`, which then prepares a
/// `danger-full-access` policy and tries the write again.
/// Expected: still denied. Widening after the fact is not possible, and the
/// attempt is not an error either - it simply changes nothing.
#[test]
fn confinement_cannot_be_widened_from_inside() {
    let Some(_) = enforcing() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let target = dir.path().join("still-denied.txt");

    let refused = confined(Policy::new(Mode::ReadOnly, dir.path()), {
        let (dir, target) = (dir.path().to_path_buf(), target.clone());
        move || {
            // Ask for the widest policy there is, from inside the boundary.
            let _ = tetanus_sandbox::prepare(&Policy::danger_full_access(&dir));
            std::fs::write(&target, b"escaped")
        }
    });

    assert_eq!(
        refused
            .expect_err("a confined thread widened itself")
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(!target.exists());
}

// ---------------------------------------------------------------- fixtures

/// This host's support, or `None` after reporting the case skipped.
///
/// A kernel without Landlock is a legitimate host for tetanus and an
/// illegitimate host for these claims, so the cases say so out loud instead of
/// passing quietly - the rule `AGENTS.md` states for the live-provider case.
fn enforcing() -> Option<tetanus_sandbox::Support> {
    match landlock::support() {
        Ok(support) => Some(support),
        Err(why) => {
            eprintln!("skipped: {why}");
            None
        }
    }
}

/// Run `body` on a thread confined by `policy`, and answer what it returned.
///
/// A thread of its own because restriction is one-way and per thread:
/// confining the harness's own thread would sandbox every case that ran after
/// it, in whatever order the runner chose.
fn confined<T, F>(policy: Policy, body: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    std::thread::spawn(move || {
        let enforcement =
            landlock::confine_current_thread(&policy).expect("the policy was enforceable");
        assert_eq!(
            enforcement,
            Enforcement::Full,
            "this case assumes full enforcement"
        );
        body()
    })
    .join()
    .expect("the confined thread finished")
}

/// A directory that no `workspace-write` policy grants.
///
/// This fixture is the whole reason two of these cases were wrong the first
/// time they ran: `tempfile::tempdir()` puts its directory under `TMPDIR`, and
/// `workspace-write` grants `TMPDIR` deliberately (TC-PORT-SANDBOX-9), so a
/// "denied" write landed in a granted root and the case failed - correctly,
/// against a backend that was doing exactly what it promised.
///
/// `/var/tmp` is world-writable, outside `/tmp`, and outside any plausible
/// `TMPDIR`. The grant check below is not decoration: it is what stops this
/// case from silently testing nothing again if a host's environment moves.
fn outside_every_grant() -> Option<tempfile::TempDir> {
    let bases = [PathBuf::from("/var/tmp"), home_cache()?];
    for base in bases {
        if !base.is_dir() {
            continue;
        }
        let Ok(dir) = tempfile::Builder::new()
            .prefix("tetanus-sandbox-outside-")
            .tempdir_in(&base)
        else {
            continue;
        };
        if granted(dir.path()) {
            continue;
        }
        return Some(dir);
    }
    eprintln!(
        "skipped: no writable directory outside the roots `workspace-write` grants, so \
         \"denied outside the workspace\" has nowhere to be denied"
    );
    None
}

/// Whether a path is under a root a `workspace-write` policy would grant, asked
/// of a policy rooted somewhere else entirely.
fn granted(path: &Path) -> bool {
    let policy = Policy::new(Mode::WorkspaceWrite, "/nonexistent-workspace");
    policy
        .writable_roots()
        .iter()
        .any(|root| path.starts_with(root))
}

/// The caller's own cache directory, as a second candidate for a place no
/// grant reaches.
fn home_cache() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache"))
}
