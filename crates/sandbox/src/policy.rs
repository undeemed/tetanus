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

    /// The mode a document named, or nothing if it named something else.
    ///
    /// The inverse of [`Mode::as_str`], and it lives beside it so the two
    /// cannot drift: a fourth mode added to the enum and not to both is a
    /// deployment whose configuration silently means something else.
    pub fn parse(word: &str) -> Option<Self> {
        [Mode::ReadOnly, Mode::WorkspaceWrite, Mode::DangerFullAccess]
            .into_iter()
            .find(|mode| mode.as_str() == word)
    }

    /// Every mode a document may name, for a refusal that lists them.
    pub const NAMES: [&'static str; 3] = ["read-only", "workspace-write", "danger-full-access"];

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
        self.writable_roots_given(std::env::var_os("TMPDIR"))
    }

    /// The same, told what `TMPDIR` is instead of reading it.
    ///
    /// The environment is process-global, so a case that set `TMPDIR` to
    /// exercise the unset and empty branches would be writing state every
    /// other case in the binary reads - the shared-mutable-state trap
    /// `AGENTS.md` records having already cost this project three red tests.
    /// Taking the value as an argument makes both branches ordinary to assert
    /// and leaves the process alone.
    fn writable_roots_given(&self, tmpdir: Option<std::ffi::OsString>) -> Vec<PathBuf> {
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
        // An unset `TMPDIR` is ordinary; an empty one is a deployment mistake,
        // and granting `PathBuf::from("")` would add a root that names the
        // current directory - a write grant nobody asked for.
        if let Some(tmpdir) = tmpdir {
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
    use std::ffi::OsString;

    #[test]
    fn a_named_root_is_writable_under_any_mode() {
        let policy = Policy::new(Mode::ReadOnly, "/work").writable("/var/state");
        assert_eq!(policy.writable_roots(), vec![PathBuf::from("/var/state")]);
    }

    /// TC-SANDBOX-POL-1: an unset or empty `TMPDIR` grants no extra root, and
    /// an empty one never grants the current directory.
    ///
    /// `PathBuf::from("")` is a relative path meaning "here", so granting it
    /// would hand a confined run write access to whatever directory it
    /// happened to start in - a grant no policy asked for and no operator
    /// could see. The unset and empty cases are different inputs and are asked
    /// separately.
    ///
    /// Input: `workspace-write` at `/work`, told `TMPDIR` is absent, empty,
    /// and set.
    /// Expected: workspace and `/tmp` in all three; nothing else when absent
    /// or empty; the named directory as well when set.
    #[test]
    fn an_absent_or_empty_tmpdir_grants_nothing_extra() {
        let policy = Policy::new(Mode::WorkspaceWrite, "/work");
        let expected = vec![PathBuf::from("/tmp"), PathBuf::from("/work")];

        assert_eq!(policy.writable_roots_given(None), expected, "absent");
        assert_eq!(
            policy.writable_roots_given(Some(OsString::from(""))),
            expected,
            "an empty TMPDIR grants nothing, and never the current directory"
        );
        assert_eq!(
            policy.writable_roots_given(Some(OsString::from("/scratch"))),
            vec![
                PathBuf::from("/scratch"),
                PathBuf::from("/tmp"),
                PathBuf::from("/work")
            ],
            "set"
        );
    }

    /// TC-SANDBOX-POL-2: two policies that mean the same thing produce the
    /// same roots, once each.
    ///
    /// The roots become kernel rules and a failure message, and a duplicate in
    /// either is a rule added twice and a message that reads as though
    /// something is wrong. Sorted and deduplicated is the stated contract.
    ///
    /// Input: a `TMPDIR` of `/tmp`, plus the same extra root named twice.
    /// Expected: three roots, sorted, no repeats.
    #[test]
    fn roots_are_deduplicated_and_sorted_however_they_arrive() {
        let policy = Policy::new(Mode::WorkspaceWrite, "/work")
            .writable("/var/state")
            .writable("/var/state");
        let roots = policy.writable_roots_given(Some(OsString::from("/tmp")));

        assert_eq!(
            roots,
            vec![
                PathBuf::from("/tmp"),
                PathBuf::from("/var/state"),
                PathBuf::from("/work")
            ]
        );
    }

    /// TC-SANDBOX-POL-3: escalation widens and refuses to do anything else.
    ///
    /// "Escalate to the mode I already have" is a mistake in what a model
    /// wrote, and a silent no-op would hide it; escalating *downwards* while
    /// reporting success would be worse - a caller believing it had narrowed a
    /// policy that had not moved.
    ///
    /// Input: every ordered pair of modes.
    /// Expected: `Some` exactly when the target is strictly wider, and the
    /// widened policy keeps every other axis.
    #[test]
    fn widening_is_the_only_direction_escalation_moves() {
        let ladder = [Mode::ReadOnly, Mode::WorkspaceWrite, Mode::DangerFullAccess];
        for from in ladder {
            for to in ladder {
                let policy = Policy::new(from, "/work");
                let widened = policy.widened_to(to);
                assert_eq!(
                    widened.is_some(),
                    to > from,
                    "{from} -> {to}: only a strictly wider mode is granted"
                );
                if let Some(widened) = widened {
                    assert_eq!(widened.mode(), to);
                    assert_eq!(widened.workspace_root(), policy.workspace_root());
                }
            }
        }
    }

    /// TC-SANDBOX-POL-4: an escalation carries the whole policy, not just the
    /// mode.
    ///
    /// An escalation widens one axis. A `widened_to` that rebuilt the policy
    /// would drop the extra roots and the network decision, so an approved
    /// escalation would quietly *narrow* the run in every other respect.
    ///
    /// Input: a policy with an extra root, a network denial and accepted
    /// partial enforcement, widened one step.
    /// Expected: every axis preserved.
    #[test]
    fn an_escalation_carries_every_other_axis_unchanged() {
        let policy = Policy::new(Mode::ReadOnly, "/work")
            .writable("/var/state")
            .network(Network::Deny)
            .accept_partial_enforcement();
        let widened = policy
            .widened_to(Mode::WorkspaceWrite)
            .expect("workspace-write is wider than read-only");

        assert_eq!(widened.network_policy(), Network::Deny);
        assert!(widened.accepts_partial());
        assert!(widened
            .writable_roots()
            .contains(&PathBuf::from("/var/state")));
        assert_eq!(widened.workspace_root(), std::path::Path::new("/work"));
    }

    /// TC-SANDBOX-POL-5: the escalation targets of a mode are exactly the
    /// modes wider than it, widest last.
    ///
    /// Input: each mode.
    /// Expected: two targets from `read-only`, one from `workspace-write`, and
    /// none at all from the widest - the boundary that stops a caller offering
    /// an escalation that cannot exist.
    #[test]
    fn the_escalation_targets_are_the_wider_modes_and_stop_at_the_top() {
        assert_eq!(
            Mode::ReadOnly.wider_modes(),
            vec![Mode::WorkspaceWrite, Mode::DangerFullAccess]
        );
        assert_eq!(
            Mode::WorkspaceWrite.wider_modes(),
            vec![Mode::DangerFullAccess]
        );
        assert!(
            Mode::DangerFullAccess.wider_modes().is_empty(),
            "there is nothing wider than unconfined to escalate to"
        );
    }

    /// TC-SANDBOX-POL-6: every mode a document may name round-trips, and a
    /// word that is not one of them is refused rather than defaulted.
    ///
    /// A settings document that misspells a mode must not quietly become the
    /// widest one. A fourth variant added to the enum is caught by `as_str`'s
    /// exhaustive match at compile time, so what needs asserting here is the
    /// round trip and the refusals.
    ///
    /// Input: every name in `Mode::NAMES`, then hostile spellings.
    /// Expected: each name parses to a mode whose `as_str` is that name;
    /// `NAMES` covers the whole enum; every other word is `None`.
    #[test]
    fn every_mode_name_round_trips_and_anything_else_is_refused() {
        for name in Mode::NAMES {
            let mode = Mode::parse(name).unwrap_or_else(|| panic!("`{name}` is a listed name"));
            assert_eq!(mode.as_str(), name);
            assert_eq!(mode.to_string(), name, "Display and as_str agree");
        }
        // `Mode::parse` is an exact compare against three literals, so one
        // spelling per class of mistake is the whole claim: a blank field, a
        // stray space, a case difference, and a plausible alias.
        for hostile in ["", " read-only", "READ-ONLY", "readonly"] {
            assert_eq!(Mode::parse(hostile), None, "`{hostile}` is not a mode");
        }
    }

    /// TC-SANDBOX-POL-7: only `danger-full-access` declines to confine.
    ///
    /// `Mode::confines` is what decides whether a backend is asked for
    /// anything at all, so a mode wrongly answering `false` here is a run with
    /// no sandbox and no error.
    ///
    /// Input: every mode.
    /// Expected: true for both confining modes, false for the named one.
    #[test]
    fn only_the_named_mode_declines_to_confine() {
        assert!(Mode::ReadOnly.confines());
        assert!(Mode::WorkspaceWrite.confines());
        assert!(!Mode::DangerFullAccess.confines());
    }

    /// TC-SANDBOX-POL-8: a policy that never said so does not accept partial
    /// enforcement.
    ///
    /// The default is the whole safety argument: a policy that silently
    /// degraded is the outcome this type exists to prevent, so the default is
    /// asserted rather than assumed.
    ///
    /// Input: a fresh policy, and one that accepted in writing.
    /// Expected: false then true.
    #[test]
    fn partial_enforcement_is_refused_until_it_is_accepted_in_writing() {
        assert!(
            !Policy::new(Mode::WorkspaceWrite, "/work").accepts_partial(),
            "the default must be to refuse a partial boundary"
        );
        assert!(Policy::new(Mode::WorkspaceWrite, "/work")
            .accept_partial_enforcement()
            .accepts_partial());
    }

    /// TC-SANDBOX-POL-9: read-only grants the sinks, and reads everywhere.
    ///
    /// A program that cannot open `/dev/null` fails in ways that look nothing
    /// like a sandbox denial, and a build that cannot read `/usr/lib` is not
    /// confined but broken. Both are stated in the module docs and neither was
    /// asserted.
    ///
    /// Input: a `read-only` policy.
    /// Expected: `/dev/null` among the sinks, `/` the only readable root, and
    /// no sink is also a writable root.
    #[test]
    fn every_mode_grants_the_sinks_and_reads_the_whole_filesystem() {
        let policy = Policy::new(Mode::ReadOnly, "/work");
        let sinks = policy.write_sinks();
        assert!(sinks.contains(&PathBuf::from("/dev/null")));
        assert!(sinks.contains(&PathBuf::from("/dev/urandom")));
        assert_eq!(policy.readable_roots(), vec![PathBuf::from("/")]);
        assert!(
            policy.writable_roots().is_empty(),
            "a sink is not a writable root: read-only grants no root at all"
        );
    }
}
