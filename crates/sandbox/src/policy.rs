//! What a confined run may touch, said once.
//!
//! A policy is a *value*: it is resolved at the boundary a call enters - a
//! tool, an RPC, a subagent - and handed down whole. Nothing below re-derives
//! it, because two derivations of "what may this write" are two answers, and
//! the day they disagree is the day a write tool cannot write a directory the
//! shell tool can.
//!
//! **The mode vocabulary is upstream's**, so a settings document written for
//! one is read by the other: `read-only`, `workspace-write`,
//! `danger-full-access` (`packages/sandbox/sandbox/src/index.ts`).
//! Two things are said here that upstream leaves to its backends. Network
//! reach is part of the policy rather than "outside this vocabulary", because
//! Landlock can govern TCP from ABI 4 and a policy that cannot express it
//! would have to be extended by every caller separately. And a policy states
//! whether a *partial* enforcement is acceptable, because the alternative -
//! deciding that per backend - is how a host silently gets less confinement
//! than the operator asked for.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What file effects a confined run may have.
///
/// The three names are upstream's, and they mean what upstream means. A
/// deployment writes one of them; a backend turns it into rules.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Read anywhere the caller could already read; write nowhere at all,
    /// except the sinks a program needs to run (`/dev/null` and friends).
    ReadOnly,
    /// `read-only`, plus writes under the workspace root and the temporary
    /// directories a build actually uses.
    WorkspaceWrite,
    /// No confinement. Named, not implied: a caller reads this word in a
    /// configuration and knows exactly what it bought.
    DangerFullAccess,
}

impl Mode {
    /// Whether this mode asks a backend for anything at all.
    pub fn confines(self) -> bool {
        self != Mode::DangerFullAccess
    }

    /// Whether `self` permits strictly more than `narrower`.
    ///
    /// The ordering is the vocabulary's own - `read-only` allows least,
    /// `danger-full-access` allows most - and it exists as a method because
    /// escalation is defined in terms of it: a retry may only ever ask for a
    /// mode that is wider, and a request to "escalate" sideways or downwards
    /// is a mistake to report rather than a widening to grant.
    pub fn is_wider_than(self, narrower: Mode) -> bool {
        self > narrower
    }

    /// The modes a call under `self` may ask to be escalated to, widest last.
    /// Upstream exports the same list as `ESCALATION_TARGETS`.
    pub fn wider_modes(self) -> Vec<Mode> {
        [Mode::ReadOnly, Mode::WorkspaceWrite, Mode::DangerFullAccess]
            .into_iter()
            .filter(|mode| mode.is_wider_than(self))
            .collect()
    }

    /// The word a document writes and a message prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::ReadOnly => "read-only",
            Mode::WorkspaceWrite => "workspace-write",
            Mode::DangerFullAccess => "danger-full-access",
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a confined run may reach the network.
///
/// Upstream's vocabulary has no network axis and its backends differ on it.
/// This says it in the policy because Landlock governs TCP from ABI 4, and a
/// deployment that wants an offline build has nowhere else to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Network {
    /// Connect and bind as the caller could already.
    Allow,
    /// No TCP connect, no TCP bind. What a backend cannot govern it must
    /// report, rather than leaving the caller believing otherwise.
    Deny,
}

/// How completely a backend enforces what it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Enforcement {
    /// Every effect the policy names is governed by the kernel.
    Full,
    /// The kernel governs some of it. A caller that needs a boundary must not
    /// read this as `Full`; a caller that asked for best effort may.
    Partial,
}

/// What one confined run may touch.
///
/// Built at the boundary and passed down. `workspace_root` is carried even
/// under modes that do not consume it, so a caller can resolve the policy once
/// and then choose an enforcement path - the same reason upstream carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    mode: Mode,
    workspace_root: PathBuf,
    /// Extra roots this run may write under, beyond what the mode grants. A
    /// caller with a state directory outside the workspace names it here
    /// rather than widening the mode.
    extra_writable: Vec<PathBuf>,
    network: Network,
    /// Whether a backend that can only govern part of this is acceptable. The
    /// default is no: a policy that silently degrades is a policy nobody can
    /// rely on, and the deployments that must run on an old kernel say so.
    accept_partial: bool,
}

impl Policy {
    /// A policy in `mode`, rooted at `workspace_root`.
    ///
    /// The root is not canonicalized here: resolution belongs to the backend,
    /// which has to compare the same spelling the kernel does, and a policy
    /// that resolved at construction would be stale by the time it is applied.
    pub fn new(mode: Mode, workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            mode,
            workspace_root: workspace_root.into(),
            extra_writable: Vec::new(),
            network: Network::Allow,
            accept_partial: false,
        }
    }

    /// The unconfined policy, spelled out. There is no `Default`: a sandbox
    /// nobody chose is exactly the mistake this type exists to prevent, so the
    /// caller writes the word.
    pub fn danger_full_access(workspace_root: impl Into<PathBuf>) -> Self {
        Self::new(Mode::DangerFullAccess, workspace_root)
    }

    pub fn writable(mut self, root: impl Into<PathBuf>) -> Self {
        self.extra_writable.push(root.into());
        self
    }

    pub fn network(mut self, network: Network) -> Self {
        self.network = network;
        self
    }

    /// Accept a backend that can only govern part of this policy.
    ///
    /// Named to be read twice, like [`Policy::danger_full_access`]. A run that
    /// sets this is telling the operator that "sandboxed" may mean less here
    /// than it does elsewhere, and [`Enforcement::Partial`] is what it gets
    /// back to prove it.
    pub fn accept_partial_enforcement(mut self) -> Self {
        self.accept_partial = true;
        self
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn network_policy(&self) -> Network {
        self.network
    }

    pub fn accepts_partial(&self) -> bool {
        self.accept_partial
    }

    /// The same policy under a wider mode, for one approved call.
    ///
    /// Widening only: a caller that asks for a mode no wider than this one
    /// gets `None`, because "escalate to the mode I already have" is a mistake
    /// in what the model wrote and a silent no-op would hide it. Everything
    /// else about the policy - the roots, the network decision, whether
    /// partial enforcement is acceptable - is carried through, because an
    /// escalation widens one axis and is not a new policy.
    pub fn widened_to(&self, mode: Mode) -> Option<Self> {
        mode.is_wider_than(self.mode).then(|| Self {
            mode,
            ..self.clone()
        })
    }

    /// The roots this run may write under: none at all under `read-only`, and
    /// under `workspace-write` the workspace, the host `/tmp`, the caller's
    /// own `TMPDIR`, and anything named explicitly.
    ///
    /// The temporary directories are in the mode's meaning rather than in each
    /// backend's spelling, because a `workspace-write` that could not write a
    /// temp file would break every compiler, every test runner and every
    /// `mktemp` a model might reasonably run - and it would break them
    /// differently on each backend. Upstream derives the same list in one
    /// place (`sandbox/src/roots.ts`) for the same reason.
    ///
    /// Deduplicated and sorted, so two policies that mean the same thing
    /// produce the same rules and a failure message reads the same twice.
    pub fn writable_roots(&self) -> Vec<PathBuf> {
        if self.mode != Mode::WorkspaceWrite {
            return self
                .extra_writable
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
        }
        let mut roots: BTreeSet<PathBuf> = BTreeSet::new();
        roots.insert(self.workspace_root.clone());
        roots.insert(PathBuf::from("/tmp"));
        if let Some(tmpdir) = std::env::var_os("TMPDIR") {
            if !tmpdir.is_empty() {
                roots.insert(PathBuf::from(tmpdir));
            }
        }
        roots.extend(self.extra_writable.iter().cloned());
        roots.into_iter().collect()
    }

    /// The write sinks every mode grants, because a program that cannot open
    /// `/dev/null` fails in ways that look nothing like a sandbox denial.
    /// Upstream grants the same sinks under `read-only`.
    pub fn write_sinks(&self) -> Vec<PathBuf> {
        vec![
            PathBuf::from("/dev/null"),
            PathBuf::from("/dev/zero"),
            PathBuf::from("/dev/full"),
            PathBuf::from("/dev/random"),
            PathBuf::from("/dev/urandom"),
            PathBuf::from("/dev/tty"),
        ]
    }

    /// The roots this run may read and execute under.
    ///
    /// The whole filesystem: confinement here is about *effects*, and a build
    /// that cannot read `/usr/lib` is not confined, it is broken. A deployment
    /// that needs reads fenced too needs a container, and `docs/parity.md`
    /// says so rather than this pretending otherwise.
    pub fn readable_roots(&self) -> Vec<PathBuf> {
        vec![PathBuf::from("/")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_grants_no_writable_root() {
        let policy = Policy::new(Mode::ReadOnly, "/work");
        assert!(policy.writable_roots().is_empty());
    }

    #[test]
    fn workspace_write_grants_the_workspace_and_the_temp_areas() {
        let policy = Policy::new(Mode::WorkspaceWrite, "/work");
        let roots = policy.writable_roots();
        assert!(roots.contains(&PathBuf::from("/work")));
        assert!(roots.contains(&PathBuf::from("/tmp")));
    }

    #[test]
    fn a_named_root_is_writable_under_any_mode() {
        let policy = Policy::new(Mode::ReadOnly, "/work").writable("/var/state");
        assert_eq!(policy.writable_roots(), vec![PathBuf::from("/var/state")]);
    }
}
