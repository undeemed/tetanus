//! Test Design Specification: the agent inbox, ported.
//!
//! Feature under test: `tetanus_turn::inbox` - the two queues that hold input
//! arriving while the loop is busy, the durable splices they are folded from,
//! and what one boundary claims. Upstream pins the same surface in
//! `packages/core/agent/src/inbox.ts`, exercised through
//! `packages/core/agent/tests/agent.spec.ts` and the agent-loop suites.
//!
//! Approach: a real JSONL journal in a temporary directory, so the durable
//! half is asserted as written bytes rather than as an intention, and a replay
//! that reads that journal back is the same code path a restart takes. One
//! case uses a journal that refuses every append, because the ordering rule -
//! durable before live - is only observable when the append fails.
//!
//! What is not restated, and why. Upstream's inbox publishes its notifications
//! through a Cordis service; these are returned from the mutation, because
//! this workspace has no notification service and a private one would be a
//! second surface competing with the journal every other reader subscribes to.
//! Its `MessageId` is a branded string on `UserMessage`; tetanus's `Message` is
//! the provider wire shape, so the id lives on the pending entry instead and
//! `PendingMessage` is what carries both. Upstream normalizes `NaN` and
//! fractional splice coordinates because a JS number can be either; `i64`
//! cannot, so the same rule is enforced by the type and the remaining
//! coordinate cases are the ones that survive it.
//!
//! Environmental needs: a writable temporary directory. No network, no clock.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value.

use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;
use tetanus_core::EventBus;
use tetanus_session::{JsonlSessionLog, SessionError, SessionEvent, SessionLog};
use tetanus_turn::inbox::{
    Inbox, InboxError, InboxNotification, InboxSplice, InboxTarget, PendingMessage, SpliceOutcome,
};
use tetanus_turn::log::topic;

/// A real journal, so every durable assertion is about a written file.
fn journal() -> (TempDir, Arc<JsonlSessionLog>) {
    let dir = TempDir::new().expect("a temporary directory");
    let log = JsonlSessionLog::create("inbox-session", dir.path().join("s.jsonl"), EventBus::new())
        .expect("a journal");
    (dir, log)
}

/// A journal that refuses everything, for the one rule that is only visible
/// when the append fails.
struct RefusingLog;

impl SessionLog for RefusingLog {
    fn id(&self) -> &str {
        "refusing"
    }
    fn append(&self, _ty: &str, _data: serde_json::Value) -> Result<SessionEvent, SessionError> {
        Err(SessionError::Io(std::io::Error::other("the disk is full")))
    }
    fn append_with_sources(
        &self,
        ty: &str,
        data: serde_json::Value,
        _sources: Vec<u64>,
    ) -> Result<SessionEvent, SessionError> {
        self.append(ty, data)
    }
    fn events(&self) -> Vec<SessionEvent> {
        Vec::new()
    }
    fn flush(&self) -> Result<(), SessionError> {
        Ok(())
    }
}

fn msg(id: &str) -> PendingMessage {
    PendingMessage::user(id, format!("content of {id}"))
}

fn ids(messages: &[PendingMessage]) -> Vec<String> {
    messages.iter().map(|m| m.id.clone()).collect()
}

/// Every `agent/inbox/spliced` record on a journal, decoded.
fn splices(log: &dyn SessionLog) -> Vec<InboxSplice> {
    log.events()
        .into_iter()
        .filter(|e| e.ty == topic::INBOX_SPLICED)
        .map(|e| serde_json::from_value(e.data).expect("a decodable splice"))
        .collect()
}

/// TC-INBOX-1: a session that has queued nothing has nothing pending.
#[test]
fn an_empty_journal_folds_to_an_empty_inbox() {
    let inbox = Inbox::replay(&[]).expect("an empty replay");
    assert!(inbox.next_turn().is_empty());
    assert!(inbox.next_step().is_empty());
    assert!(!inbox.has_pending());
}

/// TC-INBOX-2: append and prepend put a message where they say, per queue.
///
/// Two queues and not one: a prompt wanting its own turn and input joining the
/// turn already running are consumed at different boundaries, and merging them
/// would either run steering as a fresh turn or bury a prompt inside one.
#[test]
fn append_and_prepend_place_a_message_in_the_queue_they_name() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    inbox
        .append(log.as_ref(), InboxTarget::NextTurn, msg("first"))
        .expect("append");
    inbox
        .append(log.as_ref(), InboxTarget::NextTurn, msg("second"))
        .expect("append");
    inbox
        .prepend(log.as_ref(), InboxTarget::NextTurn, msg("jumped"))
        .expect("prepend");
    inbox
        .append(log.as_ref(), InboxTarget::NextStep, msg("steering"))
        .expect("append");

    assert_eq!(ids(inbox.next_turn()), ["jumped", "first", "second"]);
    assert_eq!(ids(inbox.next_step()), ["steering"]);
    assert!(inbox.has_pending());
}

/// TC-INBOX-3: a turn boundary claims all steering and exactly one prompt.
///
/// One prompt, because a person who queued three questions asked three
/// questions; draining the queue into a single turn would answer them as one
/// conversation nobody asked for. Steering is taken whole because it was aimed
/// at the turn now starting.
#[test]
fn a_turn_boundary_claims_every_steering_message_and_one_queued_turn() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    for id in ["p1", "p2"] {
        inbox
            .append(log.as_ref(), InboxTarget::NextTurn, msg(id))
            .expect("append");
    }
    for id in ["s1", "s2"] {
        inbox
            .append(log.as_ref(), InboxTarget::NextStep, msg(id))
            .expect("append");
    }

    let claimed = inbox
        .claim(log.as_ref(), InboxTarget::NextTurn, 7)
        .expect("claim");
    assert_eq!(
        ids(&claimed.messages),
        ["s1", "s2", "p1"],
        "steering first, then the one prompt the turn carries"
    );
    assert_eq!(ids(inbox.next_turn()), ["p2"]);
    assert!(inbox.next_step().is_empty());
    assert!(claimed
        .notifications
        .iter()
        .all(|n| matches!(n, InboxNotification::Claimed { turn: 7, .. })));
}

/// TC-INBOX-4: a step boundary claims steering and leaves the prompts alone.
#[test]
fn a_step_boundary_claims_steering_only() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    inbox
        .append(log.as_ref(), InboxTarget::NextTurn, msg("p1"))
        .expect("append");
    inbox
        .append(log.as_ref(), InboxTarget::NextStep, msg("s1"))
        .expect("append");

    let claimed = inbox
        .claim(log.as_ref(), InboxTarget::NextStep, 1)
        .expect("claim");
    assert_eq!(ids(&claimed.messages), ["s1"]);
    assert_eq!(ids(inbox.next_turn()), ["p1"]);
}

/// TC-INBOX-5: a claim is a deletion, a cancel is a cancellation.
///
/// The journal must be able to say which queued messages a person withdrew and
/// which the loop delivered. A claim that recorded `canceled` would tell a
/// reader the user changed their mind about a message the model went on to
/// answer.
#[test]
fn a_claim_records_a_deletion_and_a_cancel_records_a_cancellation() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    inbox
        .append(log.as_ref(), InboxTarget::NextStep, msg("claimed"))
        .expect("append");
    inbox
        .append(log.as_ref(), InboxTarget::NextTurn, msg("withdrawn"))
        .expect("append");
    inbox
        .claim(log.as_ref(), InboxTarget::NextStep, 1)
        .expect("claim");
    inbox.remove(log.as_ref(), "withdrawn").expect("remove");

    let written = splices(log.as_ref());
    let removals: Vec<Option<SpliceOutcome>> = written
        .iter()
        .filter(|s| s.removed_count.is_some())
        .map(|s| s.outcome)
        .collect();
    assert_eq!(removals, [None, Some(SpliceOutcome::Canceled)]);
}

/// TC-INBOX-6: one id may not be pending twice, in either queue.
///
/// Identity is what `replace` and `remove` name a message by, so two entries
/// answering to one name make cancelling one of them a coin toss - and the
/// person cancelling is watching a list where both look the same.
#[test]
fn a_duplicate_identity_is_refused_across_both_queues() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    inbox
        .append(log.as_ref(), InboxTarget::NextTurn, msg("only-one"))
        .expect("append");

    let same_queue = inbox.append(log.as_ref(), InboxTarget::NextTurn, msg("only-one"));
    let other_queue = inbox.append(log.as_ref(), InboxTarget::NextStep, msg("only-one"));
    assert_eq!(
        same_queue.unwrap_err(),
        InboxError::AlreadyPending("only-one".to_owned())
    );
    assert_eq!(
        other_queue.unwrap_err(),
        InboxError::AlreadyPending("only-one".to_owned())
    );
    assert_eq!(ids(inbox.next_turn()), ["only-one"]);
    assert_eq!(
        splices(log.as_ref()).len(),
        1,
        "a refused mutation writes nothing"
    );
}

/// TC-INBOX-7: replacing keeps the message's place, and may keep its name.
///
/// A person editing what they queued is editing that entry, not withdrawing it
/// and adding another at the back: the queue is the order they will be
/// answered in.
#[test]
fn a_replacement_keeps_its_place_and_may_reuse_its_own_id() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    for id in ["a", "b", "c"] {
        inbox
            .append(log.as_ref(), InboxTarget::NextTurn, msg(id))
            .expect("append");
    }

    let renamed = inbox
        .replace(log.as_ref(), "b", msg("b-edited"))
        .expect("replace")
        .expect("b was pending");
    assert_eq!(ids(inbox.next_turn()), ["a", "b-edited", "c"]);
    assert_eq!(
        renamed,
        vec![
            InboxNotification::Discarded(msg("b")),
            InboxNotification::Inserted(msg("b-edited")),
        ]
    );

    // Keeping the id is not a duplicate: the entry it would collide with is
    // the one being replaced.
    let in_place = PendingMessage::user("a", "second thoughts");
    inbox
        .replace(log.as_ref(), "a", in_place.clone())
        .expect("replace")
        .expect("a was pending");
    assert_eq!(inbox.next_turn()[0], in_place);
}

/// TC-INBOX-8: editing or cancelling something already claimed does nothing.
///
/// The window closed. Re-queueing the edit would deliver the message a second
/// time, to a model that has already read the first.
#[test]
fn replacing_or_removing_something_not_pending_writes_nothing() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    inbox
        .append(log.as_ref(), InboxTarget::NextStep, msg("gone"))
        .expect("append");
    inbox
        .claim(log.as_ref(), InboxTarget::NextStep, 1)
        .expect("claim");
    let before = splices(log.as_ref()).len();

    assert_eq!(inbox.replace(log.as_ref(), "gone", msg("edit")), Ok(None));
    assert_eq!(inbox.remove(log.as_ref(), "gone"), Ok(None));
    assert!(!inbox.has_pending());
    assert_eq!(splices(log.as_ref()).len(), before);
}

/// TC-INBOX-9: splice coordinates are normalized, and the record is the
/// normalized form.
///
/// A journal that stored `-1` would have to re-derive it at replay against a
/// queue of a different length, and reconstruct a different inbox from the
/// same events. So the normalization happens once, before the write.
#[test]
fn splice_coordinates_are_normalized_before_they_are_recorded() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    for id in ["a", "b", "c"] {
        inbox
            .append(log.as_ref(), InboxTarget::NextTurn, msg(id))
            .expect("append");
    }

    // Negative start counts back from the end; an over-long removal stops at
    // the end rather than refusing.
    let spliced = inbox
        .splice(
            log.as_ref(),
            InboxTarget::NextTurn,
            -2,
            99,
            vec![msg("replacement")],
        )
        .expect("splice");
    assert_eq!(ids(&spliced.removed), ["b", "c"]);
    assert_eq!(ids(inbox.next_turn()), ["a", "replacement"]);

    let last = splices(log.as_ref()).pop().expect("a record");
    assert_eq!(last.start, 1, "the normalized position, not -2");
    assert_eq!(last.removed_count, Some(2), "the normalized count, not 99");

    // A start past the end appends, and a negative removal removes nothing.
    inbox
        .splice(log.as_ref(), InboxTarget::NextTurn, 500, -3, vec![msg("z")])
        .expect("splice");
    assert_eq!(ids(inbox.next_turn()), ["a", "replacement", "z"]);
}

/// TC-INBOX-10: a splice that changes nothing is not a mutation.
///
/// Recording it would put a record on the journal that a replay cannot tell
/// from a bug, and publish a notification about nothing to a surface that
/// would redraw for it.
#[test]
fn a_splice_that_removes_and_inserts_nothing_writes_nothing() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    let spliced = inbox
        .splice(log.as_ref(), InboxTarget::NextStep, 0, 0, Vec::new())
        .expect("splice");
    assert!(spliced.removed.is_empty());
    assert!(spliced.notifications.is_empty());
    assert!(splices(log.as_ref()).is_empty());

    // And clearing an inbox that is already empty is the same non-event.
    assert!(inbox.clear(log.as_ref()).expect("clear").is_empty());
    assert!(splices(log.as_ref()).is_empty());
}

/// TC-INBOX-11: clearing cancels steering before prompts.
///
/// A crash between the two writes should leave the queued *prompts* behind
/// rather than steering: a prompt still means what it said, while steering
/// aimed at a turn that has since ended means nothing and cannot be delivered.
#[test]
fn clearing_cancels_steering_before_prompts() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    inbox
        .append(log.as_ref(), InboxTarget::NextTurn, msg("prompt"))
        .expect("append");
    inbox
        .append(log.as_ref(), InboxTarget::NextStep, msg("steering"))
        .expect("append");

    let notifications = inbox.clear(log.as_ref()).expect("clear");
    assert!(!inbox.has_pending());
    assert_eq!(
        notifications,
        vec![
            InboxNotification::Discarded(msg("steering")),
            InboxNotification::Discarded(msg("prompt")),
        ]
    );
    let targets: Vec<InboxTarget> = splices(log.as_ref())
        .iter()
        .filter(|s| s.removed_count.is_some())
        .map(|s| s.target)
        .collect();
    assert_eq!(targets, [InboxTarget::NextStep, InboxTarget::NextTurn]);
}

/// TC-INBOX-12: a restart folds back the queues it had.
///
/// The whole reason the queue is durable. A message typed while the agent was
/// working must survive the crash that happened before the loop reached a
/// boundary - that window is exactly what the queue exists to cover.
#[test]
fn a_replay_reconstructs_the_queues_the_session_had() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    for id in ["p1", "p2", "p3"] {
        inbox
            .append(log.as_ref(), InboxTarget::NextTurn, msg(id))
            .expect("append");
    }
    inbox
        .append(log.as_ref(), InboxTarget::NextStep, msg("s1"))
        .expect("append");
    inbox.remove(log.as_ref(), "p2").expect("remove");
    inbox
        .replace(log.as_ref(), "p3", msg("p3-edited"))
        .expect("replace");
    inbox
        .claim(log.as_ref(), InboxTarget::NextStep, 1)
        .expect("claim");

    let replayed = Inbox::replay(&log.events()).expect("a replay");
    assert_eq!(replayed, inbox);
    assert_eq!(ids(replayed.next_turn()), ["p1", "p3-edited"]);
    assert!(replayed.next_step().is_empty());
}

/// TC-INBOX-13: a forked journal folds only the events after its seed.
///
/// A fork copies its parent's events in as a seed. Folding them would put, in
/// the child, prompts a person queued for the parent - and may already have
/// seen answered there. Worse, those splices were normalized against the
/// parent's queue, so a child that folded them would be reconstructing someone
/// else's list.
#[test]
fn a_fork_folds_only_the_events_after_its_seed() {
    let (_dir, parent_log) = journal();
    let mut parent = Inbox::new();
    for id in ["p1", "p2"] {
        parent
            .append(parent_log.as_ref(), InboxTarget::NextTurn, msg(id))
            .expect("append");
    }
    let seed = parent_log.events();

    // The child is a new session with an empty queue, whose own splice is
    // normalized against that empty queue.
    let (_child_dir, child_log) = journal();
    let mut child = Inbox::new();
    child
        .append(child_log.as_ref(), InboxTarget::NextTurn, msg("childs-own"))
        .expect("append");

    let forked: Vec<SessionEvent> = seed.iter().cloned().chain(child_log.events()).collect();
    let folded = Inbox::replay(&forked[seed.len()..]).expect("a replay of the child's own events");
    assert_eq!(ids(folded.next_turn()), ["childs-own"]);

    // And a reader that folded the seed too would answer with a queue the
    // child never had - not merely the parent's entries as well, but in an
    // order neither session ever saw, because the child's start of 0 was
    // normalized against its own empty queue and lands in front of them.
    let whole = Inbox::replay(&forked).expect("a replay of everything");
    assert_eq!(ids(whole.next_turn()), ["childs-own", "p1", "p2"]);
    assert_ne!(whole, folded);
}

/// TC-INBOX-14: a journal record that does not apply refuses the replay.
///
/// Skipping it would be worse than refusing: every later coordinate in the
/// journal was normalized against a list the skipping reader no longer has, so
/// a "recovered" inbox would be a different queue presented as the session's.
/// The seq is named because the journal is the only place to look.
#[test]
fn an_invalid_persisted_splice_refuses_the_replay_and_names_its_seq() {
    let (_dir, log) = journal();
    log.append(
        topic::INBOX_SPLICED,
        json!({"target": "next-turn", "start": 0, "inserted": [msg("a")]}),
    )
    .expect("append");
    // Removing two from a queue of one cannot have happened.
    let damaged = log
        .append(
            topic::INBOX_SPLICED,
            json!({"target": "next-turn", "start": 0, "removed_count": 2}),
        )
        .expect("append");

    match Inbox::replay(&log.events()) {
        Err(InboxError::InvalidPersisted { seq, reason }) => {
            assert_eq!(seq, damaged.seq);
            assert!(reason.contains("queue of 1"), "{reason}");
        }
        other => panic!("expected a refusal naming the seq, got {other:?}"),
    }
}

/// TC-INBOX-15: a journal that refuses the append leaves the queues alone.
///
/// The ordering rule, and the only way to see it. Serving a message from
/// memory that no restart will remember is worse than never accepting it: the
/// person watched it join the queue and would have no reason to type it again.
#[test]
fn a_refused_append_leaves_the_queues_untouched() {
    let mut inbox = Inbox::new();
    let refused = inbox.append(&RefusingLog, InboxTarget::NextTurn, msg("lost"));
    assert!(
        matches!(refused, Err(InboxError::Journal(_))),
        "{refused:?}"
    );
    assert!(!inbox.has_pending());
}

/// TC-INBOX-16: what a mutation publishes, and in what order.
///
/// A surface redraws from these, so a discard that arrived after its
/// replacement would flash the old message back over the new one.
#[test]
fn a_mutation_publishes_what_it_discarded_before_what_it_inserted() {
    let (_dir, log) = journal();
    let mut inbox = Inbox::new();
    inbox
        .append(log.as_ref(), InboxTarget::NextStep, msg("old"))
        .expect("append");
    let spliced = inbox
        .splice(
            log.as_ref(),
            InboxTarget::NextStep,
            0,
            1,
            vec![msg("new-a"), msg("new-b")],
        )
        .expect("splice");
    assert_eq!(
        spliced.notifications,
        vec![
            InboxNotification::Discarded(msg("old")),
            InboxNotification::Inserted(msg("new-a")),
            InboxNotification::Inserted(msg("new-b")),
        ]
    );
}
