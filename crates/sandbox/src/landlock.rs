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

use crate::policy::{Enforcement, Mode, Network, Policy};
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
    let errno = std::io::Error::last_os_error();
    Err(SandboxError::Unavailable {
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
    })
}

/// What this host can enforce, and how completely.
pub fn support() -> Result<Support, SandboxError> {
    let abi = abi_version()?;
    Ok(Support {
        backend: "landlock",
        abi: Some(abi),
        // Everything below ABI 4 cannot govern TCP at all; that only matters
        // for a policy that asks, so the judgement is made per policy in
        // `prepare` rather than declared here.
        governs_network: abi >= 4,
        governs_truncate: abi >= 3,
        governs_ioctl: abi >= 5,
    })
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

    // A policy that asks for something this kernel cannot govern is refused
    // unless the caller said it would accept less. This is the degraded-kernel
    // path: loud at the boundary, never a quiet downgrade.
    let mut missing: Vec<&'static str> = Vec::new();
    if policy.network_policy() == Network::Deny && !support.governs_network {
        missing.push("network denial (needs Landlock ABI 4)");
    }
    if !support.governs_truncate {
        missing.push("truncation of an existing file (needs Landlock ABI 3)");
    }
    let enforcement = if missing.is_empty() {
        Enforcement::Full
    } else {
        Enforcement::Partial
    };
    if enforcement == Enforcement::Partial && !policy.accepts_partial() {
        return Err(SandboxError::Degraded {
            backend: "landlock",
            abi,
            missing: missing.join(", "),
        });
    }

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
    // Before ABI 4 the struct had no network field, and passing the larger
    // size to an older kernel is E2BIG.
    let size = if abi >= 4 {
        std::mem::size_of::<RulesetAttr>()
    } else {
        std::mem::size_of::<u64>()
    };
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

/// The mode a policy asks for, as a sentence for a diagnostic.
pub fn described(policy: &Policy) -> String {
    match policy.mode() {
        Mode::ReadOnly => "read-only".to_string(),
        Mode::WorkspaceWrite => format!(
            "workspace-write under {}",
            policy.workspace_root().display()
        ),
        Mode::DangerFullAccess => "unconfined".to_string(),
    }
}
