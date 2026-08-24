//! Which sessions a given session started as subagents.
//!
//! A parent needs to enumerate what it delegated: to show it, to wait on it, to
//! stop it. The durable answer is on disk — every child's journal names the
//! session that started it — so this is a fold over enumerated headers rather
//! than anything a live registry has to remember.
//!
//! # Two lineages, and why one field could not carry both
//!
//! Contract section 4.4.9 puts two separate facts on `session/start`, and this
//! module is the reason they are separate. `parent_session` says the journal
//! whose history this one was **copied from** — a fork. `spawned_by` says the
//! session that **started** this one as a subagent. A fork is a second way of
//! continuing one conversation; a subagent is a different conversation another
//! one asked for.
//!
//! Filtering on parentage would report every fork a person made by hand as an
//! agent the harness had started on their behalf. Collapsing the two into one
//! field with a kind beside it is worse, and the contract rules it out for the
//! case that breaks it: a session can be both. A fork of a subagent's journal
//! is still that subagent's work, so it inherits `spawned_by` while its own
//! `parent_session` names the journal it was copied from — and a listing must
//! report it under the session that started the work, not lose it.
//!
//! So delegation is exactly the `spawned_by` edge, and nothing here consults
//! parentage to decide what to *report*.
//!
//! # A copy is still walked through
//!
//! Traversal is the other question. A subagent started from a *fork* of this
//! conversation is work this conversation led to, and a walk that followed
//! only `spawned_by` would stop at the fork and lose the subtree under it. So
//! the walk crosses both edges and emits only what delegation created —
//! upstream's rule, in a vocabulary that keeps the two edges apart.
//!
//! # The tree is read from files, so it may not be a tree
//!
//! Headers come off disk, written by other processes and editable by anyone
//! with the directory. A lineage chain can therefore contain a cycle, or point
//! at a session that is not present, or be longer than a recursive walk's
//! stack. Every walk here is iterative and bounded, and every unresolvable
//! link ends it, because the alternative is a parent hanging — or aborting —
//! while it lists its children.
//!
//! # What this is not
//!
//! Upstream's listing also *materializes* each row: a mode and label read from
//! the child's `subagent/descriptor` through the projection registry, and a
//! diagnostic row for a settled child whose descriptor cannot be read. That
//! half needs the projection registry and persistence inspection wired
//! together, which this engine does not expose yet; [`crate::descriptor`]
//! already carries the record it reads. What is here is candidate selection,
//! tree position and order — the part that is a fold over headers and nothing
//! else.
//!
//! Parity: upstream `packages/subagent/subagent/src/list-children.ts`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

/// One enumerated session, as much of its header as this fold needs.
///
/// Deliberately not the engine's header type: this crate does not depend on
/// the engine, and the caller that enumerates journals is the one that knows
/// how to read them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRecord {
    /// The session's own id.
    pub session_id: String,
    /// The session whose history this one was copied from — a fork
    /// (contract section 4.4.6). Not delegation, and never reported as it.
    pub parent_session: Option<String>,
    /// The session that started this one as a subagent (contract
    /// section 4.4.9). This, and only this, is the delegation edge.
    pub spawned_by: Option<String>,
    /// When the session was created, which is the order a listing is in.
    ///
    /// Not a header field: it is the `session/start` event's own timestamp,
    /// which the engine already reports as `SessionInfo::created_time`. A
    /// journal that recorded its creation twice could disagree with itself.
    pub created_at: u64,
    /// How many levels of delegation deep the header says it is.
    ///
    /// Absolute and durable, and not the same number as
    /// [`Descendant::distance`], which counts edges from whichever session a
    /// walk was asked about.
    pub depth: Option<u64>,
    /// Whether it is open in this process right now — upstream's `running`
    /// versus `inactive`. Supplied by the caller, because liveness is not a
    /// fact of the journal and no fold over headers can discover it.
    pub live: bool,
}

impl ChildRecord {
    /// Whether `parent` started this session as a subagent.
    fn spawned_by_session(&self, parent: &str) -> bool {
        self.spawned_by.as_deref() == Some(parent)
    }
}

/// One delegated session's place below the session a walk started from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descendant<'a> {
    /// The session itself.
    pub record: &'a ChildRecord,
    /// The session that started it — its `spawned_by`, the durable fact,
    /// rather than whichever edge the walk happened to arrive by. It need not
    /// be the session the walk was asked about.
    pub spawned_by: &'a str,
    /// How many lineage edges lie between it and the session the walk started
    /// from; a direct child is 1.
    ///
    /// Relative to that starting session, so it is not the header's
    /// [`ChildRecord::depth`]: asking a child of a child what it delegated
    /// answers 1, while those sessions' persisted depths are 2 and 3.
    pub distance: u64,
}

/// Siblings are listed oldest first, ties broken on id.
///
/// Creation time rather than enumeration order, because enumeration order is
/// whatever the directory or the backend happened to return, and a listing a
/// person reads twice should not reorder itself. The id breaks the tie so the
/// order is total: two sessions can be created in the same millisecond.
fn by_creation(left: &ChildRecord, right: &ChildRecord) -> Ordering {
    left.created_at
        .cmp(&right.created_at)
        .then_with(|| left.session_id.cmp(&right.session_id))
}

/// The subagents `parent` started, oldest first.
pub fn direct_children<'a>(records: &'a [ChildRecord], parent: &str) -> Vec<&'a ChildRecord> {
    let mut found: Vec<&ChildRecord> = records
        .iter()
        .filter(|record| record.spawned_by_session(parent))
        .collect();
    found.sort_by(|left, right| by_creation(left, right));
    found
}

/// Whether `session` started anything — upstream's `hasChildren`.
///
/// Answered without building the listing, because a tree listing asks this of
/// every node it reports and the answer is one predicate over the corpus.
pub fn has_children(records: &[ChildRecord], session: &str) -> bool {
    records
        .iter()
        .any(|record| record.spawned_by_session(session))
}

/// Every delegated session below `root`, in stable pre-order, with the session
/// that started it and its distance from the root.
///
/// The walk crosses both lineage edges — what a session started, and what was
/// copied from it — and reports only delegated work, so a subagent started
/// from a fork of this conversation is still found. It is iterative rather
/// than recursive: a lineage chain is as deep as a directory of files says it
/// is, and a walk that overflowed the stack would take the process with it. It
/// is bounded by a visited set that starts holding the root, so a chain that
/// loops back terminates and the root is never reported as its own descendant.
pub fn descendants<'a>(records: &'a [ChildRecord], root: &'a str) -> Vec<Descendant<'a>> {
    // Both edges, because emission and traversal are different questions: a
    // fork is not delegated work, but work delegated from it descends from
    // whatever the fork descends from.
    let mut edges: BTreeMap<&str, Vec<&ChildRecord>> = BTreeMap::new();
    for record in records {
        for parent in [
            record.spawned_by.as_deref(),
            record.parent_session.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            edges.entry(parent).or_default().push(record);
        }
    }
    for siblings in edges.values_mut() {
        // A session carrying both edges from the *same* session would
        // otherwise be queued twice; the visited set would drop the second,
        // but not before it had taken a slot in sibling order.
        siblings.sort_by(|left, right| by_creation(left, right));
        siblings.dedup_by(|left, right| left.session_id == right.session_id);
    }

    let mut found = Vec::new();
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    visited.insert(root);

    // A stack, pushed youngest first so the oldest sibling is popped first:
    // that makes the emitted order the pre-order a reader expects, a session
    // immediately before its own subtree.
    let mut stack: Vec<(&'a ChildRecord, u64)> = Vec::new();
    let push_children = |stack: &mut Vec<(&'a ChildRecord, u64)>, parent: &str, distance: u64| {
        for child in edges.get(parent).into_iter().flatten().rev() {
            stack.push((child, distance));
        }
    };
    push_children(&mut stack, root, 1);

    while let Some((record, distance)) = stack.pop() {
        let id = record.session_id.as_str();
        // A session already visited is a cycle, a diamond, or a fork of
        // something already below the root; walking it again would report it
        // twice at best and never terminate at worst.
        if !visited.insert(id) {
            continue;
        }
        if let Some(spawned_by) = record.spawned_by.as_deref() {
            found.push(Descendant {
                record,
                spawned_by,
                distance,
            });
        }
        push_children(&mut stack, id, distance + 1);
    }
    found
}
