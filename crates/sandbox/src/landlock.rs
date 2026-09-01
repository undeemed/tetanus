//! The Linux backend: Landlock, the same mechanism upstream's native helper
//! uses.
//!
//! **Why the syscalls are made by hand.** There is a crate for this, and it is
//! a good one, but it is built around restricting *the thread that calls it*.
//! What this needs is the fork/exec split: the ruleset is built in the parent,
//! where allocating and opening directories is safe, and only two
//! allocation-free syscalls run in the child between `fork` and `exec`. After
//! `fork` in a process with threads, a child that allocates can deadlock on a
//! lock another thread held at the instant of the fork - so the child's half
//! is deliberately three system calls and no library code.
//!
//! **Deny by default is what the ABI already does.** A Landlock ruleset names
//! the access rights it *handles*; anything handled and not granted by a rule
//! is denied. So the handled set is everything this kernel knows about, and
//! the rules are the allow-list the policy describes. Adding a right to the
//! handled set can only ever remove permission, which is why an unknown-to-us
//! kernel right is a gap to report rather than a hole to ignore.
//!
//! **A kernel that cannot do this says so.** `landlock_create_ruleset` with
//! the version flag answers the ABI level, and an ABI of zero, an `ENOSYS`, or
//! a kernel built without the LSM enabled all come back as
//! [`SandboxError::Unavailable`]. Nothing falls back to "not confined": a
//! policy that asked for confinement and did not get it is an error at the
//! boundary, before anything runs.
//!
//! Parity: upstream `packages/sandbox/sandbox-local` (its Landlock dialect and
//! its probe), restated against this seam.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use crate::policy::{Enforcement, Network, Policy};
use crate::{Confinement, SandboxError, Support};

// The three Landlock system calls, by number. They have no libc wrappers.
const SYS_CREATE_RULESET: libc::c_long = 444;
const SYS_ADD_RULE: libc::c_long = 445;
const SYS_RESTRICT_SELF: libc::c_long = 446;

/// Ask for the ABI version rather than for a ruleset.
const CREATE_RULESET_VERSION: u32 = 1;
/// `LANDLOCK_RULE_PATH_BENEATH`.
const RULE_PATH_BENEATH: u32 = 1;
/// `LANDLOCK_RULE_NET_PORT`, from ABI 4. Declared for the shape; this backend
/// governs network by handling the access and granting no port rule.
#[allow(dead_code)]
const RULE_NET_PORT: u32 = 2;

/// File access rights, by the ABI that introduced them.
mod access {
    pub const EXECUTE: u64 = 1 << 0;
    pub const WRITE_FILE: u64 = 1 << 1;
    pub const READ_FILE: u64 = 1 << 2;
    pub const READ_DIR: u64 = 1 << 3;
    pub const REMOVE_DIR: u64 = 1 << 4;
    pub const REMOVE_FILE: u64 = 1 << 5;
    pub const MAKE_CHAR: u64 = 1 << 6;
    pub const MAKE_DIR: u64 = 1 << 7;
    pub const MAKE_REG: u64 = 1 << 8;
    pub const MAKE_SOCK: u64 = 1 << 9;
    pub const MAKE_FIFO: u64 = 1 << 10;
    pub const MAKE_BLOCK: u64 = 1 << 11;
    pub const MAKE_SYM: u64 = 1 << 12;
    /// ABI 2.
    pub const REFER: u64 = 1 << 13;
    /// ABI 3.
    pub const TRUNCATE: u64 = 1 << 14;
    /// ABI 5.
    pub const IOCTL_DEV: u64 = 1 << 15;

    /// Everything an ABI-1 kernel knows.
    pub const ABI1: u64 = EXECUTE
        | WRITE_FILE
        | READ_FILE
        | READ_DIR
        | REMOVE_DIR
        | REMOVE_FILE
        | MAKE_CHAR
        | MAKE_DIR
        | MAKE_REG
        | MAKE_SOCK
        | MAKE_FIFO
        | MAKE_BLOCK
        | MAKE_SYM;

    /// The rights a reader needs, and nothing that changes anything.
    pub const READ: u64 = READ_FILE | READ_DIR | EXECUTE;

    /// The rights a writer needs under a granted root.
    pub const WRITE: u64 = WRITE_FILE
        | MAKE_REG
        | MAKE_DIR
        | MAKE_SYM
        | MAKE_FIFO
        | MAKE_SOCK
        | MAKE_CHAR
        | MAKE_BLOCK
        | REMOVE_FILE
        | REMOVE_DIR;

    /// Writing to an existing sink such as `/dev/null`: the bytes go
    /// somewhere, but nothing is created, removed or renamed.
    pub const SINK: u64 = WRITE_FILE;
}

/// Network access rights, from ABI 4.
mod net {
    pub const BIND_TCP: u64 = 1 << 0;
    pub const CONNECT_TCP: u64 = 1 << 1;
    pub const ALL: u64 = BIND_TCP | CONNECT_TCP;
}

/// `struct landlock_ruleset_attr`. The net field arrived in ABI 4, and the
/// size passed to the syscall is what tells the kernel which shape it is.
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
}

/// `struct landlock_path_beneath_attr`, which the kernel declares packed.
#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: RawFd,
}

/// The ABI level this kernel speaks, or why it speaks none.
pub fn abi_version() -> Result<u32, SandboxError> {
    // Safety: the version query passes a null attribute with a zero size, as
    // the kernel documents; it changes nothing and only reads back a number.
    let answered = unsafe {
        libc::syscall(
            SYS_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            CREATE_RULESET_VERSION,
        )
    };
    if answered > 0 {
        return Ok(answered as u32);
    }
    Err(unavailable(std::io::Error::last_os_error()))
}

/// Why this kernel offers no Landlock, in the words an operator can act on.
///
/// Separated from the syscall so the three answers can be asserted on a host
/// that has Landlock. Every machine in this fleet does, so inside
/// [`abi_version`] this mapping was unreachable and untested - and it is the
/// text an operator reads when confinement is unavailable, which is exactly
/// when nobody wants it to be wrong.
fn unavailable(errno: std::io::Error) -> SandboxError {
    SandboxError::Unavailable {
        backend: "landlock",
        why: match errno.raw_os_error() {
            Some(libc::ENOSYS) => {
                "this kernel has no Landlock system calls (built before 5.13, or without \
                 CONFIG_SECURITY_LANDLOCK)"
                    .to_string()
            }
            Some(libc::EOPNOTSUPP) => {
                "Landlock is compiled into this kernel but not enabled; add `landlock` to the \
                 `lsm=` boot parameter"
                    .to_string()
            }
            _ => format!("the kernel refused a Landlock version query: {errno}"),
        },
    }
}

/// What this host can enforce, and how completely.
pub fn support() -> Result<Support, SandboxError> {
    Ok(support_at(abi_version()?))
}

/// What a kernel speaking `abi` can enforce.
///
/// Taken as an argument rather than probed, because the interesting values are
/// the ones this machine is not: a host at ABI 1 governs no truncation and no
/// TCP, and the refusal that depends on it ([`verdict`]) is unreachable on
/// any modern kernel. A function that probes cannot be asked about a kernel it
/// is not running on; this one can.
fn support_at(abi: u32) -> Support {
    Support {
        backend: "landlock",
        abi: Some(abi),
        // Everything below ABI 4 cannot govern TCP at all; that only matters
        // for a policy that asks, so the judgement is made per policy in
        // `verdict` rather than declared here.
        governs_network: abi >= 4,
        governs_truncate: abi >= 3,
        governs_ioctl: abi >= 5,
    }
}

/// The handled-access sets for one kernel: everything it knows about, so
/// anything not granted below is denied.
fn handled(abi: u32, network: Network) -> (u64, u64) {
    let mut fs = access::ABI1;
    if abi >= 2 {
        fs |= access::REFER;
    }
    if abi >= 3 {
        fs |= access::TRUNCATE;
    }
    if abi >= 5 {
        fs |= access::IOCTL_DEV;
    }
    // Handling a network right with no rule granting it is how TCP is denied.
    // Handling nothing is how it is left alone: a ruleset that handled the
    // rights and granted them all would be the same thing more expensively,
    // but it would also refuse on a kernel that cannot express the grant.
    let net = match network {
        Network::Deny if abi >= 4 => net::ALL,
        _ => 0,
    };
    (fs, net)
}

/// Build the ruleset for `policy` in this process, ready to be applied in a
/// child.
///
/// Everything expensive happens here: the directories are opened, the rules
/// are added, and any failure is reported to the caller that can still do
/// something about it. What crosses `fork` is one file descriptor.
pub fn prepare(policy: &Policy) -> Result<Confinement, SandboxError> {
    let support = support()?;
    let abi = support.abi.unwrap_or(0);

    let enforcement = verdict(&support, policy)?;

    let (handled_fs, handled_net) = handled(abi, policy.network_policy());
    let ruleset = create_ruleset(abi, handled_fs, handled_net)?;

    // Read and execute everywhere: confinement here is about effects. A
    // build that cannot read `/usr/lib` is not confined, it is broken.
    for root in policy.readable_roots() {
        allow(&ruleset, &root, access::READ, Requirement::Required)?;
    }
    for root in policy.writable_roots() {
        // A granted root that does not exist yet is not a mistake worth
        // failing a run over - a workspace is created before it is used, and a
        // `TMPDIR` may be per-user - but it grants nothing until it does.
        allow(
            &ruleset,
            &root,
            access::READ | access::WRITE | truncate_bit(abi),
            Requirement::Optional,
        )?;
    }
    for sink in policy.write_sinks() {
        allow(&ruleset, &sink, access::SINK, Requirement::Optional)?;
    }

    Ok(Confinement {
        backend: "landlock",
        enforcement,
        ruleset: Some(ruleset),
        denial_hints: &["Permission denied", "EACCES", "os error 13"],
    })
}

/// How completely `support` can enforce `policy`, or the refusal to try.
///
/// What the kernel cannot govern, and whether the caller said in writing that
/// it would accept less: one decision, so one function. A policy asking for
/// more than this kernel has is refused loud at the boundary, never downgraded
/// quietly. Split out of [`prepare`] because inside it that refusal is
/// reachable only by running on an old kernel - so on this fleet, and on CI,
/// it was never once executed.
fn verdict(support: &Support, policy: &Policy) -> Result<Enforcement, SandboxError> {
    let mut missing: Vec<&'static str> = Vec::new();
    if policy.network_policy() == Network::Deny && !support.governs_network {
        missing.push("network denial (needs Landlock ABI 4)");
    }
    if !support.governs_truncate {
        missing.push("truncation of an existing file (needs Landlock ABI 3)");
    }
    if missing.is_empty() {
        return Ok(Enforcement::Full);
    }
    if policy.accepts_partial() {
        return Ok(Enforcement::Partial);
    }
    Err(SandboxError::Degraded {
        backend: "landlock",
        abi: support.abi.unwrap_or(0),
        missing: missing.join(", "),
    })
}

/// Whether a granted root has to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Requirement {
    /// A root the policy cannot work without: failing to open it is a fault.
    Required,
    /// A root that grants nothing until it exists.
    Optional,
}

fn truncate_bit(abi: u32) -> u64 {
    if abi >= 3 {
        access::TRUNCATE
    } else {
        0
    }
}

fn create_ruleset(abi: u32, handled_fs: u64, handled_net: u64) -> Result<OwnedFd, SandboxError> {
    let attr = RulesetAttr {
        handled_access_fs: handled_fs,
        handled_access_net: handled_net,
    };
    let size = attr_size(abi);
    // Safety: `attr` outlives the call, `size` describes exactly the prefix of
    // it this ABI defines, and the flags are zero as the kernel requires for a
    // real ruleset.
    let fd = unsafe { libc::syscall(SYS_CREATE_RULESET, &attr as *const RulesetAttr, size, 0u32) };
    if fd < 0 {
        return Err(SandboxError::Kernel {
            backend: "landlock",
            what: "create a ruleset",
            source: std::io::Error::last_os_error(),
        });
    }
    // Safety: the kernel returned a fresh descriptor this call now owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// How much of [`RulesetAttr`] this ABI defines.
///
/// Before ABI 4 the struct had no network field, and passing the larger size
/// to an older kernel is `E2BIG` - a refusal to create any ruleset at all,
/// which on a host that could have been confined is the worst outcome
/// available. Separated so both sides of the boundary are asserted without an
/// old kernel to run on.
fn attr_size(abi: u32) -> usize {
    if abi >= 4 {
        std::mem::size_of::<RulesetAttr>()
    } else {
        std::mem::size_of::<u64>()
    }
}

/// Grant `rights` beneath `path`.
fn allow(
    ruleset: &OwnedFd,
    path: &Path,
    rights: u64,
    requirement: Requirement,
) -> Result<(), SandboxError> {
    if rights == 0 {
        return Ok(());
    }
    let Some(spelling) = path.to_str().and_then(|text| CString::new(text).ok()) else {
        return Err(SandboxError::Path {
            path: path.display().to_string(),
            why: "a granted root has to be a path without an interior null byte".to_string(),
        });
    };
    // `O_PATH` opens the directory as a reference without reading it, which is
    // exactly what a rule needs and is all the permission the grant requires.
    // Safety: the string is null-terminated and lives across the call.
    let dirfd = unsafe { libc::open(spelling.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if dirfd < 0 {
        let error = std::io::Error::last_os_error();
        return match requirement {
            Requirement::Optional => {
                tracing::debug!(path = %path.display(), %error, "a granted root is not there yet");
                Ok(())
            }
            Requirement::Required => Err(SandboxError::Path {
                path: path.display().to_string(),
                why: format!("could not be opened to grant access: {error}"),
            }),
        };
    }
    // Safety: the descriptor came from `open` above and is closed below on
    // every path.
    let dirfd = unsafe { OwnedFd::from_raw_fd(dirfd) };

    let attr = PathBeneathAttr {
        allowed_access: rights,
        parent_fd: dirfd.as_raw_fd(),
    };
    // Safety: the attribute matches the packed layout the kernel declares for
    // a path-beneath rule, and both descriptors are open for the call.
    let added = unsafe {
        libc::syscall(
            SYS_ADD_RULE,
            ruleset.as_raw_fd(),
            RULE_PATH_BENEATH,
            &attr as *const PathBeneathAttr,
            0u32,
        )
    };
    if added < 0 {
        return Err(SandboxError::Kernel {
            backend: "landlock",
            what: "add a path rule",
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(())
}

/// Apply a prepared ruleset to *this* thread and everything it goes on to
/// start.
///
/// This is the child's half, and it is deliberately three system calls with no
/// allocation and no library code between them: it runs after `fork` in a
/// process that has threads, where anything holding a lock at the instant of
/// the fork is held for ever.
///
/// It is one-way. A thread that has restricted itself cannot widen its own
/// rights again, which is what makes it safe to do in a child that is about to
/// `exec` something a model chose.
///
/// # Safety
///
/// The caller must hold `ruleset` open, and must be prepared for every later
/// operation on this thread to be governed by it.
pub unsafe fn restrict_this_thread(ruleset: RawFd) -> Result<(), std::io::Error> {
    // Landlock refuses to restrict a thread that could still gain privileges
    // through a set-uid program, because that would be a way out.
    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if libc::syscall(SYS_RESTRICT_SELF, ruleset, 0u32) != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Restrict the calling thread for the rest of its life, for a caller that is
/// confining *itself* rather than a child.
///
/// The filesystem service is the intended caller, and it is not written yet;
/// `docs/parity.md` records that as the next slice. It is here because
/// it is the same three syscalls, and because a policy that can only be
/// applied to children would quietly become "the shell is confined and the
/// write tool is not".
pub fn confine_current_thread(policy: &Policy) -> Result<Enforcement, SandboxError> {
    if !policy.mode().confines() {
        return Ok(Enforcement::Full);
    }
    let confinement = prepare(policy)?;
    let enforcement = confinement.enforcement;
    let Some(ruleset) = confinement.ruleset.as_ref() else {
        return Ok(enforcement);
    };
    // Safety: the ruleset is open for the call, and confining the calling
    // thread is what this function is for.
    unsafe { restrict_this_thread(ruleset.as_raw_fd()) }.map_err(|source| {
        SandboxError::Kernel {
            backend: "landlock",
            what: "restrict this thread",
            source,
        }
    })?;
    Ok(enforcement)
}

#[cfg(test)]
mod tests {
    //! Test Design Specification: the Landlock backend's kernel-dependent
    //! decisions, asked of every ABI from one host.
    //!
    //! Features under test: the ABI-to-capability mapping, the degraded-kernel
    //! refusal, the syscall struct size, and the unavailability diagnostic.
    //!
    //! Approach and why it is not the integration suite's. Every one of these
    //! decisions turns on the ABI level of the running kernel, and this fleet -
    //! and the CI runner - run kernels capable enough that
    //! `crates/sandbox/tests/upstream_sandbox.rs` TC-PORT-SANDBOX-7 takes its
    //! "this kernel can govern it" early return. Measured on 2026-09-01, that
    //! left the whole refusal path at zero coverage: both `verdict` pushes,
    //! `Enforcement::Partial`, `SandboxError::Degraded`, and
    //! `Policy::accepts_partial` were never once executed by the suite. The
    //! functions here take the ABI and the support as arguments precisely so
    //! that a host cannot decide what gets tested.
    //!
    //! These are the *decisions*. The kernel is still asked to enforce them in
    //! the integration suite, which is the only place that can prove a denial
    //! is real - a policy object asserting about itself proves nothing, which
    //! is the rule `ARCHITECTURE.md` §4.10 states.
    //!
    //! Environmental needs: none required. The decision cases make no syscall
    //! at all; TC-SANDBOX-ABI-10 through -12 do touch the kernel - one confines
    //! a thread of its own, two build a real ruleset so that a refusal is the
    //! path's and not the ruleset's - and those report themselves skipped on a
    //! host without Landlock rather than passing on nothing.
    //!
    //! Pass criteria: each case's stated expected value exactly.
    //! Fail criteria: any other value, or a panic.

    use super::*;
    use crate::policy::Mode;

    /// TC-SANDBOX-ABI-1: every capability flips on at its own ABI and not one
    /// level earlier.
    ///
    /// The failure this prevents is an off-by-one in a security capability
    /// table: a host at ABI 3 believed to govern TCP denies nothing and says
    /// it denies everything. Both sides of every boundary are asked, which is
    /// what makes `>=` distinguishable from `>`.
    ///
    /// Input: ABI 0 through 6.
    /// Expected: each flag false strictly below its arrival level and true at
    /// and above it.
    #[test]
    fn each_capability_arrives_at_its_own_abi_and_not_before() {
        for abi in 0..=6u32 {
            let support = support_at(abi);
            assert_eq!(support.backend, "landlock");
            assert_eq!(support.abi, Some(abi), "the level is reported as given");
            assert_eq!(support.governs_truncate, abi >= 3, "truncate at ABI {abi}");
            assert_eq!(support.governs_network, abi >= 4, "network at ABI {abi}");
            assert_eq!(support.governs_ioctl, abi >= 5, "ioctl at ABI {abi}");
        }
    }

    /// TC-SANDBOX-ABI-2: an ABI-1 kernel cannot govern truncation, and a
    /// policy that did not accept less is refused rather than downgraded.
    ///
    /// This is the case that could not previously exist. It is the quiet
    /// failure the crate's own module docs name: a deployment moves to an
    /// older kernel, the policy stops being enforceable, and the run still
    /// works so nothing says so.
    ///
    /// Input: `workspace-write`, network allowed, against ABI 1 and ABI 2.
    /// Expected: `Degraded` naming the backend, the ABI, and truncation.
    #[test]
    fn a_kernel_below_abi_3_refuses_a_policy_that_did_not_accept_less() {
        let policy = Policy::new(Mode::WorkspaceWrite, "/work");
        for abi in [0, 1, 2] {
            let refused = verdict(&support_at(abi), &policy)
                .expect_err("a kernel that cannot govern truncation must refuse");
            let SandboxError::Degraded {
                backend,
                abi: said,
                missing,
            } = refused
            else {
                panic!("the refusal must be `Degraded`, got {refused:?}");
            };
            assert_eq!(backend, "landlock");
            assert_eq!(said, abi, "the refusal names the ABI it measured");
            assert!(
                missing.contains("truncation"),
                "the refusal names what is missing: {missing}"
            );
            assert!(
                missing.contains("ABI 3"),
                "and what would fix it: {missing}"
            );
        }
    }

    /// TC-SANDBOX-ABI-3: below ABI 4 a network denial is named as missing too,
    /// and both shortfalls are reported together.
    ///
    /// One refusal listing one of two missing capabilities would send an
    /// operator to fix half the problem and meet the same refusal again.
    ///
    /// Input: `workspace-write` denying the network, at ABI 3 (truncation
    /// arrived, TCP has not) and at ABI 1 (neither has).
    /// Expected: ABI 3 names only the network; ABI 1 names both.
    #[test]
    fn a_refusal_names_every_missing_capability_not_the_first() {
        let strict = Policy::new(Mode::WorkspaceWrite, "/work").network(Network::Deny);

        let at_three = refusal(3, &strict);
        assert!(at_three.contains("network denial"), "{at_three}");
        assert!(
            !at_three.contains("truncation"),
            "ABI 3 governs truncation, so it is not missing: {at_three}"
        );

        let at_one = refusal(1, &strict);
        assert!(at_one.contains("network denial"), "{at_one}");
        assert!(
            at_one.contains("truncation"),
            "neither capability exists, so both are named: {at_one}"
        );
    }

    /// What `verdict` says is missing when it refuses `policy` at `abi`.
    fn refusal(abi: u32, policy: &Policy) -> String {
        match verdict(&support_at(abi), policy) {
            Err(SandboxError::Degraded { missing, .. }) => missing,
            other => panic!("ABI {abi} must refuse this policy, got {other:?}"),
        }
    }

    /// TC-SANDBOX-ABI-4: a policy that allows the network is not refused for
    /// TCP it never asked to deny.
    ///
    /// The complement of TC-SANDBOX-ABI-3, and the boundary from the other
    /// side: a shortfall computed from the kernel alone would refuse every
    /// policy on an old host, including the ones it can serve completely.
    ///
    /// Input: `workspace-write` with `Network::Allow` at ABI 3 and ABI 5.
    /// Expected: nothing missing at either, and `Full` enforcement.
    #[test]
    fn a_policy_that_does_not_deny_the_network_is_not_refused_for_tcp() {
        let relaxed = Policy::new(Mode::WorkspaceWrite, "/work").network(Network::Allow);
        for abi in [3, 4, 5, 6] {
            assert_eq!(
                verdict(&support_at(abi), &relaxed)
                    .unwrap_or_else(|why| panic!("ABI {abi} governs all this asks: {why}")),
                Enforcement::Full
            );
        }
    }

    /// TC-SANDBOX-ABI-5: accepting partial enforcement in writing converts the
    /// refusal into `Partial`, and never into `Full`.
    ///
    /// Reporting `Full` here would be the single worst outcome in this crate:
    /// a caller that asked whether the boundary was real, and was told yes by
    /// a kernel that cannot make it real.
    ///
    /// Input: the ABI-1 policy of TC-SANDBOX-ABI-2, with
    /// `accept_partial_enforcement`.
    /// Expected: `Ok(Partial)` - not an error, and not `Full`.
    #[test]
    fn accepting_less_in_writing_yields_partial_and_never_full() {
        let accepted = Policy::new(Mode::WorkspaceWrite, "/work").accept_partial_enforcement();
        assert!(accepted.accepts_partial());

        let enforcement = verdict(&support_at(1), &accepted)
            .expect("partial enforcement was accepted in writing");
        assert_eq!(
            enforcement,
            Enforcement::Partial,
            "a kernel that cannot govern truncation must never report a full boundary"
        );
    }

    /// TC-SANDBOX-ABI-6: the ruleset attribute is sized to the ABI, because
    /// the larger shape is `E2BIG` on an older kernel.
    ///
    /// Input: ABI 3 and ABI 4.
    /// Expected: one `u64` below 4, the whole struct at and above it, and the
    /// two sizes actually differ - a struct that gained no field would make
    /// this case pass while proving nothing.
    #[test]
    fn the_ruleset_attribute_is_sized_to_the_abi() {
        assert!(
            attr_size(4) > attr_size(3),
            "the network field has to make the ABI-4 shape larger, or this \
             boundary is not a boundary"
        );
        for abi in 0..=3 {
            assert_eq!(attr_size(abi), std::mem::size_of::<u64>(), "ABI {abi}");
        }
        for abi in 4..=6 {
            assert_eq!(
                attr_size(abi),
                std::mem::size_of::<RulesetAttr>(),
                "ABI {abi}"
            );
        }
    }

    /// TC-SANDBOX-ABI-7: the handled-access set only ever grows with the ABI,
    /// and network rights are handled only when the policy denies them.
    ///
    /// Deny-by-default is the whole mechanism: a right the ruleset does not
    /// *handle* is a right the kernel does not govern, so a handled set that
    /// shrank as the ABI rose would silently un-govern an effect. The
    /// monotonicity is asserted rather than the exact bits, because the bits
    /// are the kernel's to name and the ordering is this function's to keep.
    ///
    /// Input: every ABI 1..=6 under both network decisions.
    /// Expected: the filesystem set is a superset of every lower ABI's; TCP
    /// rights are handled from ABI 4 under `Deny` and never under `Allow`.
    #[test]
    fn the_handled_set_grows_with_the_abi_and_tcp_only_when_denied() {
        let mut previous_fs = 0u64;
        for abi in 1..=6u32 {
            let (fs, net) = handled(abi, Network::Allow);
            assert_eq!(
                fs & previous_fs,
                previous_fs,
                "ABI {abi} handles less than ABI {} did, which un-governs an effect",
                abi - 1
            );
            previous_fs = fs;
            assert_eq!(net, 0, "ABI {abi}: an allowed network handles no TCP right");

            let (denied_fs, denied_net) = handled(abi, Network::Deny);
            assert_eq!(
                denied_fs, fs,
                "the network decision must not move the fs set"
            );
            assert_eq!(
                denied_net,
                if abi >= 4 { net::ALL } else { 0 },
                "ABI {abi}: TCP is handled from 4 and cannot be handled before it"
            );
        }
        // The specific bits the ABI table promises, from both sides.
        assert_eq!(handled(1, Network::Allow).0 & access::REFER, 0);
        assert_eq!(handled(2, Network::Allow).0 & access::REFER, access::REFER);
        assert_eq!(handled(2, Network::Allow).0 & access::TRUNCATE, 0);
        assert_eq!(
            handled(3, Network::Allow).0 & access::TRUNCATE,
            access::TRUNCATE
        );
        assert_eq!(handled(4, Network::Allow).0 & access::IOCTL_DEV, 0);
        assert_eq!(
            handled(5, Network::Allow).0 & access::IOCTL_DEV,
            access::IOCTL_DEV
        );
    }

    /// TC-SANDBOX-ABI-8: the truncate right is granted exactly when the kernel
    /// knows it.
    ///
    /// Granting a bit an ABI does not define is `EINVAL` from `add_rule`,
    /// which fails the whole `prepare` - so this boundary decides whether an
    /// old host is confined or refused outright.
    ///
    /// Input: ABI 0..=6.
    /// Expected: zero below 3, the truncate bit at and above it.
    #[test]
    fn the_truncate_right_is_granted_exactly_when_the_kernel_knows_it() {
        for abi in 0..=2 {
            assert_eq!(truncate_bit(abi), 0, "ABI {abi} has no truncate right");
        }
        for abi in 3..=6 {
            assert_eq!(truncate_bit(abi), access::TRUNCATE, "ABI {abi}");
        }
    }

    /// TC-SANDBOX-ABI-9: an unavailable Landlock says which of the three
    /// reasons it is, in words that name the fix.
    ///
    /// An operator reading this line is already in the failure case, and the
    /// three causes have three different remedies: rebuild the kernel, change
    /// a boot parameter, or read the errno. A single generic sentence sends
    /// all three to the wrong place. This mapping was unreachable on any host
    /// that has Landlock, which is every host in this fleet.
    ///
    /// Input: `ENOSYS`, `EOPNOTSUPP`, and an unrelated errno.
    /// Expected: three distinct messages, each naming its own remedy, all
    /// reported as `Unavailable` from the `landlock` backend.
    #[test]
    fn an_unavailable_landlock_names_which_reason_it_is() {
        let cases = [
            (libc::ENOSYS, "CONFIG_SECURITY_LANDLOCK", "5.13"),
            (libc::EOPNOTSUPP, "lsm=", "boot parameter"),
        ];
        for (errno, must_name, also) in cases {
            let SandboxError::Unavailable { backend, why } =
                unavailable(std::io::Error::from_raw_os_error(errno))
            else {
                panic!("errno {errno} must report the backend unavailable");
            };
            assert_eq!(backend, "landlock");
            assert!(why.contains(must_name), "errno {errno} said: {why}");
            assert!(why.contains(also), "errno {errno} said: {why}");
        }

        // Anything else carries the errno through rather than guessing.
        let SandboxError::Unavailable { why, .. } =
            unavailable(std::io::Error::from_raw_os_error(libc::EPERM))
        else {
            panic!("an unexpected errno is still an unavailable backend");
        };
        assert!(
            why.contains("refused a Landlock version query"),
            "an unrecognised errno is reported as itself: {why}"
        );
        assert!(
            !why.contains("CONFIG_SECURITY_LANDLOCK"),
            "and is not misattributed to a cause it is not: {why}"
        );
    }

    /// TC-SANDBOX-ABI-10: an unconfining policy asks the kernel for nothing.
    ///
    /// Input: `danger-full-access` through `confine_current_thread`.
    /// Expected: `Full` - the boundary the caller asked for is the absence of
    /// one, and it is completely enforced - with no ruleset created. Run on a
    /// thread of its own because restriction is one-way and would otherwise
    /// confine every case that followed.
    #[test]
    fn an_unconfining_policy_restricts_nothing_and_reports_full() {
        let answered =
            std::thread::spawn(|| confine_current_thread(&Policy::danger_full_access("/work")))
                .join()
                .expect("the thread finished");
        assert_eq!(
            answered.expect("an unconfined policy cannot fail"),
            Enforcement::Full
        );
    }

    /// TC-SANDBOX-ABI-11: a granted root whose spelling contains a NUL is
    /// refused by name, not passed to the kernel.
    ///
    /// A path with an interior NUL cannot become a `CString`, and the failure
    /// mode this prevents is silence: an early `return Ok(())` would drop the
    /// grant and confine a run more tightly than its policy said, which looks
    /// like a broken command rather than a rejected path.
    ///
    /// Input: a writable root containing a NUL byte.
    /// Expected: `SandboxError::Path` quoting the path and saying why.
    #[test]
    #[cfg(unix)]
    fn a_granted_root_containing_a_nul_is_refused_by_name() {
        use std::os::unix::ffi::OsStrExt;
        let hostile = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/work/a\0b"));

        // A real ruleset, so the refusal is the path's and not the ruleset's.
        let Some(ruleset) = ruleset_or_skip() else {
            return;
        };

        let refused = allow(&ruleset, &hostile, access::READ, Requirement::Required)
            .expect_err("a path with an interior NUL cannot be granted");
        let SandboxError::Path { path, why } = refused else {
            panic!("the refusal must name the path, got {refused:?}");
        };
        assert!(path.contains("work"), "the refusal quotes the path: {path}");
        assert!(why.contains("null byte"), "and says why: {why}");
    }

    /// TC-SANDBOX-ABI-12: an absent root is optional or fatal according to its
    /// requirement, and granting no rights touches nothing at all.
    ///
    /// The two requirements exist for two real cases - a `TMPDIR` that is
    /// per-user and may not exist, versus `/` which must - and collapsing them
    /// either fails every run on a host without a `TMPDIR` or silently skips a
    /// root the policy depended on. The zero-rights guard belongs with them
    /// because `truncate_bit` returns zero below ABI 3, so a caller composes an
    /// empty right set without meaning to, and an empty `path_beneath` rule is
    /// `EINVAL` - a `prepare` failing with nothing wrong with it.
    ///
    /// Input: one path that does not exist, under both requirements and with
    /// no rights.
    /// Expected: `Ok` for `Optional`; `SandboxError::Path` naming the path for
    /// `Required`; and `Ok` for no rights even under `Required`, which can only
    /// mean the guard returned before the `open`.
    #[test]
    fn an_absent_root_is_optional_or_fatal_and_no_rights_touches_nothing() {
        let Some(ruleset) = ruleset_or_skip() else {
            return;
        };
        let absent = std::path::Path::new("/nonexistent-tetanus-sandbox-probe");
        assert!(!absent.exists(), "the fixture depends on this being absent");

        allow(&ruleset, absent, access::READ, Requirement::Optional)
            .expect("a root that is not there yet grants nothing and is not a fault");

        allow(&ruleset, absent, 0, Requirement::Required)
            .expect("no rights is nothing to grant, so the path is never opened");

        let refused = allow(&ruleset, absent, access::READ, Requirement::Required)
            .expect_err("a required root that cannot be opened is a fault");
        let SandboxError::Path { path, why } = refused else {
            panic!("the refusal must name the path, got {refused:?}");
        };
        assert!(path.contains("nonexistent-tetanus-sandbox-probe"));
        assert!(why.contains("could not be opened"), "{why}");
    }

    /// A real ruleset, or `None` after reporting the case skipped.
    ///
    /// The cases that call `allow` need one so a refusal is the path's and not
    /// the ruleset's, and a host without Landlock cannot make one.
    fn ruleset_or_skip() -> Option<OwnedFd> {
        let Ok(abi) = abi_version() else {
            eprintln!("skipped: no Landlock on this host, so there is no ruleset to add a rule to");
            return None;
        };
        Some(
            create_ruleset(abi, handled(abi, Network::Allow).0, 0).expect("a ruleset for the test"),
        )
    }
}
