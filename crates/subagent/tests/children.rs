//! Conformance: which sessions a session started as subagents.
//!
//! Feature under test: `tetanus_subagent::children` — the durable half of
//! listing what a parent delegated, folded from enumerated journal headers.
//!
//! Ported from the enumeration half of upstream
//! `packages/subagent/subagent/tests/list-children.spec.ts`. Case ids
//! TC-SUB-KIDS-1..14.
//!
//! One adaptation runs through every case. Upstream filters on one parent link
//! plus `origin === 'subagent'`; contract section 4.4.9 splits the two facts
//! into `parent_session` (copied from) and `spawned_by` (started by), for the
//! case a single origin field cannot represent — a fork of a subagent's
//! journal is both. So delegation here is exactly the `spawned_by` edge, and
//! TC-SUB-KIDS-4 is the case that would fail under the collapsed shape.
//!
//! Upstream's other half — materializing each row's mode and label through the
//! projection registry, and diagnosing a child whose descriptor cannot be read
//! — waits on a projection-plus-persistence surface this engine does not
//! expose; nothing here changes when it lands.

use tetanus_subagent::children::{descendants, direct_children, has_children, ChildRecord};

/// A session `spawner` started as a subagent.
fn child(id: &str, spawner: &str, created_at: u64) -> ChildRecord {
    ChildRecord {
        session_id: id.to_owned(),
        parent_session: None,
        spawned_by: Some(spawner.to_owned()),
        created_at,
        depth: Some(1),
        live: false,
    }
}

/// A session forked from `source`: its history was copied, nobody started it.
fn forked(id: &str, source: &str, created_at: u64) -> ChildRecord {
    ChildRecord {
        parent_session: Some(source.to_owned()),
        spawned_by: None,
        depth: None,
        ..child(id, source, created_at)
    }
}

/// A session nobody delegated or forked.
fn root(id: &str) -> ChildRecord {
    ChildRecord {
        session_id: id.to_owned(),
        parent_session: None,
        spawned_by: None,
        created_at: 0,
        depth: None,
        live: false,
    }
}

fn ids(records: &[&ChildRecord]) -> Vec<String> {
    records.iter().map(|r| r.session_id.clone()).collect()
}

/// Every reported descendant as `(id, spawned_by, distance)`.
fn tree(records: &[ChildRecord], root: &str) -> Vec<(String, String, u64)> {
    descendants(records, root)
        .into_iter()
        .map(|d| {
            (
                d.record.session_id.clone(),
                d.spawned_by.to_owned(),
                d.distance,
            )
        })
        .collect()
}

/// TC-SUB-KIDS-1: a session that started nothing lists nothing.
#[test]
fn a_session_that_delegated_nothing_lists_nothing() {
    let records = [root("p")];
    assert!(direct_children(&records, "p").is_empty());
    assert!(descendants(&records, "p").is_empty());
    assert!(!has_children(&records, "p"));
}

/// TC-SUB-KIDS-2: direct children are the sessions this one started.
#[test]
fn direct_children_are_the_sessions_this_one_started() {
    let records = [
        root("p"),
        child("a", "p", 1),
        child("b", "p", 2),
        child("grandchild", "a", 3),
        root("unrelated"),
    ];
    assert_eq!(ids(&direct_children(&records, "p")), ["a", "b"]);
    assert_eq!(ids(&direct_children(&records, "a")), ["grandchild"]);
    assert!(direct_children(&records, "unrelated").is_empty());
}

/// TC-SUB-KIDS-3: a fork is not a delegated child.
///
/// A fork is a second way of continuing one conversation, not an agent the
/// harness started on the user's behalf. Reporting it as one shows a person
/// something they did not do.
#[test]
fn a_fork_of_this_session_is_not_a_child_of_it() {
    let records = [
        root("p"),
        child("delegated", "p", 1),
        forked("copy", "p", 2),
    ];
    assert_eq!(ids(&direct_children(&records, "p")), ["delegated"]);
    assert_eq!(
        tree(&records, "p"),
        [("delegated".to_owned(), "p".to_owned(), 1)]
    );
}

/// TC-SUB-KIDS-4: a fork of a subagent's journal is still that subagent's
/// work.
///
/// The case that decided the field shape. Contract section 4.4.9: a fork
/// inherits the origin facts it is a copy of, so this session carries a
/// `spawned_by` naming the session that started the original *and* a
/// `parent_session` naming the journal it was copied from. Under one origin
/// field with a kind beside it, this session is either a fork — and its
/// spawner loses it — or a subagent, and the copy lineage is lost. Both facts
/// are here, and the listing reports it under the session that started the
/// work.
#[test]
fn a_fork_of_a_delegated_journal_is_still_listed_under_its_spawner() {
    let mut copy_of_child = forked("copy-of-a", "a", 3);
    copy_of_child.spawned_by = Some("p".to_owned());
    copy_of_child.depth = Some(1);
    let records = [root("p"), child("a", "p", 1), copy_of_child];

    assert_eq!(ids(&direct_children(&records, "p")), ["a", "copy-of-a"]);
    assert_eq!(
        ids(&direct_children(&records, "a")),
        Vec::<String>::new(),
        "copying a's journal is not a spawning a did"
    );
}

/// TC-SUB-KIDS-5: children come back oldest first, ties broken on id.
///
/// Enumeration order is whatever the directory or the backend returned, so a
/// listing ordered by it would reshuffle itself between two reads of the same
/// unchanged corpus. The id breaks the tie because two children can be created
/// in the same millisecond and an order that is not total is not an order.
#[test]
fn children_are_ordered_by_creation_time_then_id() {
    let records = [
        child("late", "p", 30),
        child("b-tie", "p", 10),
        child("early", "p", 1),
        child("a-tie", "p", 10),
    ];
    assert_eq!(
        ids(&direct_children(&records, "p")),
        ["early", "a-tie", "b-tie", "late"]
    );
}

/// TC-SUB-KIDS-6: the tree comes back in pre-order with spawner and distance.
#[test]
fn the_tree_is_flattened_in_pre_order_with_its_spawner_and_distance() {
    let records = [
        root("p"),
        child("a", "p", 1),
        child("b", "p", 2),
        child("a1", "a", 3),
        child("a1x", "a1", 4),
    ];
    assert_eq!(
        tree(&records, "p"),
        [
            ("a".to_owned(), "p".to_owned(), 1),
            ("a1".to_owned(), "a".to_owned(), 2),
            ("a1x".to_owned(), "a1".to_owned(), 3),
            ("b".to_owned(), "p".to_owned(), 1),
        ]
    );
}

/// TC-SUB-KIDS-7: an agent started from a fork is still discovered.
///
/// The rule that separates *reported* from *traversed*. A fork is not
/// delegated work, but work delegated from it descends from whatever the fork
/// descends from — and a walk following only the delegation edge would stop at
/// the fork and lose a running agent, silently, which is the worst way to lose
/// one. The fork is not reported, and the distance counts its edge because
/// that is the distance in the lineage that exists.
#[test]
fn an_agent_started_from_a_fork_is_still_discovered() {
    let records = [
        root("p"),
        forked("copy", "p", 1),
        child("under-copy", "copy", 2),
    ];
    assert!(direct_children(&records, "p").is_empty());
    assert_eq!(
        tree(&records, "p"),
        [("under-copy".to_owned(), "copy".to_owned(), 2)]
    );
}

/// TC-SUB-KIDS-8: the persisted depth is not the walk's distance.
///
/// One is absolute and durable — how deep in delegation this session sits,
/// which is what bounds further delegation across a resume. The other is
/// relative to whichever session was asked. Conflating them would let a
/// listing rendered from a mid-tree session report a budget nobody has.
#[test]
fn the_persisted_depth_is_not_the_distance_from_the_asked_session() {
    let mut deep = child("grandchild", "a", 2);
    deep.depth = Some(2);
    let records = [root("p"), child("a", "p", 1), deep];

    let found = descendants(&records, "a");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].distance, 1, "one edge from the session asked");
    assert_eq!(found[0].record.depth, Some(2), "two levels of delegation");
}

/// TC-SUB-KIDS-9: a cycle terminates and never reports the root.
///
/// Headers are files in a directory, written by other processes and editable
/// by anyone; a lineage chain that loops is reachable without anything in this
/// process going wrong, and a parent that hung while listing what it had
/// delegated would be the worst possible response.
#[test]
fn a_cycle_in_the_lineage_terminates_without_revisiting_the_root() {
    let records = [child("a", "b", 1), child("b", "a", 2)];
    assert_eq!(
        tree(&records, "a"),
        [("b".to_owned(), "a".to_owned(), 1)],
        "b is reached once and a is never its own descendant"
    );
}

/// TC-SUB-KIDS-10: a session pointing at itself terminates too.
///
/// This port's own, and the shortest cycle there is — the one an off-by-one in
/// a spawner would produce.
#[test]
fn a_session_that_started_itself_terminates() {
    let records = [child("loop", "loop", 1)];
    assert!(descendants(&records, "loop").is_empty());
}

/// TC-SUB-KIDS-11: a deep chain is walked without consuming the call stack.
///
/// The walk is iterative for this reason alone: the depth of a lineage chain
/// is decided by a directory of files, not by anything this process chose, and
/// a recursive walk would turn a deep — or maliciously deep — corpus into a
/// stack overflow, which aborts the process rather than failing the call.
#[test]
fn a_deeply_nested_chain_does_not_consume_the_call_stack() {
    let depth = 50_000u64;
    let mut records = vec![root("p")];
    let mut parent = "p".to_owned();
    for step in 1..=depth {
        let id = format!("s{step}");
        // Every other link is a fork, so the walk is proving it descends
        // through copy edges at this depth as well.
        records.push(if step % 2 == 0 {
            child(&id, &parent, step)
        } else {
            forked(&id, &parent, step)
        });
        parent = id;
    }
    let found = descendants(&records, "p");
    assert_eq!(found.len() as u64, depth / 2);
    assert_eq!(found.last().expect("the deepest child").distance, depth);
}

/// TC-SUB-KIDS-12: `has_children` counts delegation only.
///
/// It is the flag a listing shows against a row to say the subtree is worth
/// opening, so a fork below a child must not set it — a reader told to expand
/// and shown nothing learns to distrust the flag.
#[test]
fn has_children_counts_delegated_children_only() {
    let records = [
        root("p"),
        child("delegating", "p", 1),
        child("childless", "p", 2),
        child("grandchild", "delegating", 3),
        forked("copy", "childless", 4),
    ];
    assert!(has_children(&records, "delegating"));
    assert!(!has_children(&records, "childless"));
    assert!(!has_children(&records, "nobody-has-heard-of-this"));
}

/// TC-SUB-KIDS-13: a link to a session that is not there is still a link.
///
/// This port's own. A child whose spawner's journal was deleted is ordinary
/// housekeeping, not corruption: it is still listed against the name it
/// records, because the name is what the journal says, and it is simply not
/// reachable from any root that exists.
#[test]
fn a_link_to_a_missing_session_is_still_listed_by_name() {
    let records = [root("p"), child("orphan", "deleted-spawner", 1)];
    assert_eq!(
        ids(&direct_children(&records, "deleted-spawner")),
        ["orphan"]
    );
    assert_eq!(
        tree(&records, "deleted-spawner"),
        [("orphan".to_owned(), "deleted-spawner".to_owned(), 1)]
    );
    assert!(descendants(&records, "p").is_empty());
}

/// TC-SUB-KIDS-14: liveness is carried through, not inferred.
///
/// This port's own. Whether a child is open in this process is not a fact of
/// its journal — it is upstream's `running` versus `inactive` — so the fold
/// must neither invent it nor drop it: a parent deciding whether to wait on a
/// child reads exactly what the caller supplied.
#[test]
fn liveness_is_carried_through_untouched() {
    let mut running = child("a", "p", 1);
    running.live = true;
    let records = [root("p"), running, child("b", "p", 2)];

    let listed = direct_children(&records, "p");
    assert_eq!(
        listed.iter().map(|r| r.live).collect::<Vec<_>>(),
        [true, false]
    );
    assert!(descendants(&records, "p")[0].record.live);
}
