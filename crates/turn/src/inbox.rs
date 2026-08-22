//! The queue of input waiting for a boundary the loop has not reached yet.
//!
//! A person types while the agent is working. That message cannot go into the
//! request in flight — the model is already reading a fixed list — and it must
//! not be dropped, so it waits in one of two queues and is claimed at the next
//! boundary that can carry it. `next-turn` holds prompts that each want a turn
//! of their own; `next-step` holds input that joins the turn already running,
//! which is how steering works and where a tool's post-execute context lands.
//!
//! # Why it is durable, and what that costs
//!
//! Every mutation is one `agent/inbox/spliced` event, and the queues are a
//! replay-once projection of those events. A queue held only in memory loses a
//! person's typing to a crash between the moment they pressed return and the
//! moment the loop reached a boundary — the exact window the queue exists to
//! cover. So the durable event commits **before** the projection moves: a
//! journal that refused the append leaves the queues exactly as they were,
//! rather than serving a message no restart will remember.
//!
//! # Splice coordinates are normalized once, here
//!
//! The mutation is a splice, with the coordinate rules of one: a negative
//! start counts from the end, an over-long delete stops at the end, and a
//! start past the end appends. That is upstream's surface and a caller writes
//! against it. What is recorded is always the **normalized** result, never
//! what the caller said, because a replay that had to re-derive `-1` against a
//! list of a different length would reconstruct a different queue from the
//! same journal.
//!
//! # A pending message is identified, and identity is unique
//!
//! Replacing or cancelling one queued message needs to name it, so a pending
//! entry carries an id. The id is on the entry rather than on
//! [`crate::llm::Message`], which is the provider wire shape: putting a
//! harness's queue key on every request message would send it to a model that
//! has no use for it. No id may be pending twice, across *both* queues
//! together — two entries answering to one name make `replace` and `remove`
//! ambiguous, and an ambiguous cancel is how a person cancels the wrong
//! message.
//!
//! # A forked journal folds its own suffix only
//!
//! A fork copies its parent's events in as a seed. Those splices are the
//! parent's queue, so folding them would resurrect, in the child, prompts a
//! person queued for the parent and may already have seen answered. The
//! caller passes the suffix; [`Inbox::replay`] folds exactly what it is given.
//!
//! Parity: upstream `packages/core/agent/src/inbox.ts`.

use serde::{Deserialize, Serialize};
use serde_json::json;
use tetanus_session::{SessionError, SessionEvent, SessionLog};

use crate::llm::Message;
use crate::log::topic;

/// Which queue a mutation is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InboxTarget {
    /// Prompts each awaiting a turn of their own.
    NextTurn,
    /// Input joining the turn already running, at its next step boundary.
    NextStep,
}

/// One queued message and the identity a caller cancels or replaces it by.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingMessage {
    pub id: String,
    pub message: Message,
}

impl PendingMessage {
    pub fn user(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            message: Message::user(content),
        }
    }
}

/// Why a splice removed what it removed.
///
/// Recorded only for a cancellation, so a reader of the journal can tell a
/// person changing their mind from the loop consuming what was waiting. A
/// claim is a pure deletion and writes no outcome at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpliceOutcome {
    Canceled,
}

/// One normalized mutation, exactly as it is written to the journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxSplice {
    pub target: InboxTarget,
    /// Normalized position: always within `0..=len` of the target queue as it
    /// stood before the splice.
    pub start: usize,
    /// Normalized removal count. Omitted when nothing was removed, so an
    /// insertion's record does not carry a zero that reads like a decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inserted: Vec<PendingMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<SpliceOutcome>,
}

impl InboxSplice {
    fn removed(&self) -> usize {
        self.removed_count.unwrap_or(0)
    }
}

/// What a mutation published, in the order it happened.
///
/// Returned rather than dispatched: this module owns the queue, and a surface
/// that wants to show a message arriving or being cancelled subscribes to the
/// journal like every other reader.
#[derive(Debug, Clone, PartialEq)]
pub enum InboxNotification {
    /// A message joined a queue.
    Inserted(PendingMessage),
    /// A message was cancelled before anything claimed it.
    Discarded(PendingMessage),
    /// A message was claimed by the turn that will carry it.
    Claimed { message: PendingMessage, turn: u64 },
}

/// What the inbox refuses, and why.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum InboxError {
    /// An id already pending, in either queue. Named, because a caller that
    /// queued the same message twice needs to know which one it was.
    #[error("message {0:?} is already pending")]
    AlreadyPending(String),
    /// Coordinates that do not fit the queue they name. Unreachable through
    /// [`Inbox::splice`], which normalizes first; a journal is the only source
    /// of a splice nobody normalized against this queue.
    #[error("inbox splice does not fit its queue: {0}")]
    OutOfRange(String),
    /// A persisted splice that does not apply to the queue the earlier events
    /// built. The seq is named because the journal is the only place to look.
    #[error("invalid persisted inbox splice at session seq {seq}: {reason}")]
    InvalidPersisted { seq: u64, reason: String },
    /// The journal refused the append, so nothing moved.
    #[error("the journal refused an inbox splice: {0}")]
    Journal(String),
}

impl From<SessionError> for InboxError {
    fn from(error: SessionError) -> Self {
        InboxError::Journal(error.to_string())
    }
}

/// The two queues, folded from `agent/inbox/spliced`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Inbox {
    next_turn: Vec<PendingMessage>,
    next_step: Vec<PendingMessage>,
}

impl Inbox {
    /// A queue with nothing in it, for a session that has never queued.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold the queues out of a journal's own events.
    ///
    /// A record that does not apply refuses the whole replay rather than being
    /// skipped: the queues after a skipped splice are not the queues the
    /// session had, and every later coordinate in the journal was normalized
    /// against a list this reader no longer has.
    pub fn replay(events: &[SessionEvent]) -> Result<Self, InboxError> {
        let mut inbox = Self::new();
        for event in events {
            if event.ty != topic::INBOX_SPLICED {
                continue;
            }
            let splice: InboxSplice = serde_json::from_value(event.data.clone()).map_err(|e| {
                InboxError::InvalidPersisted {
                    seq: event.seq,
                    reason: e.to_string(),
                }
            })?;
            inbox
                .check(&splice)
                .map_err(|error| InboxError::InvalidPersisted {
                    seq: event.seq,
                    reason: error.to_string(),
                })?;
            inbox.apply(&splice);
        }
        Ok(inbox)
    }

    /// Prompts awaiting turns of their own.
    pub fn next_turn(&self) -> &[PendingMessage] {
        &self.next_turn
    }

    /// Input awaiting the next step boundary.
    pub fn next_step(&self) -> &[PendingMessage] {
        &self.next_step
    }

    /// Whether either queue holds work.
    pub fn has_pending(&self) -> bool {
        !self.next_turn.is_empty() || !self.next_step.is_empty()
    }

    fn queue(&self, target: InboxTarget) -> &Vec<PendingMessage> {
        match target {
            InboxTarget::NextTurn => &self.next_turn,
            InboxTarget::NextStep => &self.next_step,
        }
    }

    fn queue_mut(&mut self, target: InboxTarget) -> &mut Vec<PendingMessage> {
        match target {
            InboxTarget::NextTurn => &mut self.next_turn,
            InboxTarget::NextStep => &mut self.next_step,
        }
    }

    /// Put one message at the end of a queue.
    pub fn append(
        &mut self,
        log: &dyn SessionLog,
        target: InboxTarget,
        message: PendingMessage,
    ) -> Result<Vec<InboxNotification>, InboxError> {
        let end = self.queue(target).len() as i64;
        self.splice(log, target, end, 0, vec![message])
            .map(|outcome| outcome.notifications)
    }

    /// Put one message at the front of a queue, ahead of what is already
    /// waiting.
    pub fn prepend(
        &mut self,
        log: &dyn SessionLog,
        target: InboxTarget,
        message: PendingMessage,
    ) -> Result<Vec<InboxNotification>, InboxError> {
        self.splice(log, target, 0, 0, vec![message])
            .map(|outcome| outcome.notifications)
    }

    /// Swap one pending message for another, keeping its place in the queue.
    ///
    /// Answers `false` when nothing was pending under that id, and writes
    /// nothing: a caller editing a message the loop has already claimed has
    /// missed its window, and inventing a queue entry for it would deliver the
    /// edit twice.
    pub fn replace(
        &mut self,
        log: &dyn SessionLog,
        id: &str,
        message: PendingMessage,
    ) -> Result<Option<Vec<InboxNotification>>, InboxError> {
        let Some((target, index)) = self.locate(id) else {
            return Ok(None);
        };
        self.splice(log, target, index as i64, 1, vec![message])
            .map(|outcome| Some(outcome.notifications))
    }

    /// Cancel one pending message. Answers `false` when it was not pending.
    pub fn remove(
        &mut self,
        log: &dyn SessionLog,
        id: &str,
    ) -> Result<Option<Vec<InboxNotification>>, InboxError> {
        let Some((target, index)) = self.locate(id) else {
            return Ok(None);
        };
        self.splice(log, target, index as i64, 1, Vec::new())
            .map(|outcome| Some(outcome.notifications))
    }

    /// Cancel everything pending.
    ///
    /// `next-step` first, then `next-turn`: a crash between the two leaves the
    /// queued *turns* behind rather than the steering for a turn that may
    /// already have ended, and a prompt that survives is recoverable where
    /// steering aimed at a finished turn is not.
    pub fn clear(&mut self, log: &dyn SessionLog) -> Result<Vec<InboxNotification>, InboxError> {
        let mut notifications = Vec::new();
        for target in [InboxTarget::NextStep, InboxTarget::NextTurn] {
            let len = self.queue(target).len() as i64;
            let outcome = self.splice(log, target, 0, len, Vec::new())?;
            notifications.extend(outcome.notifications);
        }
        Ok(notifications)
    }

    /// Take the batch one boundary carries, in the order the loop sends it.
    ///
    /// Always the whole of `next-step`; plus exactly one queued turn when the
    /// boundary is a turn boundary, because a turn carries one prompt and
    /// draining the queue here would run several prompts as one conversation
    /// nobody asked for.
    ///
    /// The splices are pure deletions with no `canceled` outcome: what the
    /// loop consumed is not what a person cancelled, and a journal that
    /// conflated them could not tell a reader which messages were ever
    /// delivered.
    pub fn claim(
        &mut self,
        log: &dyn SessionLog,
        target: InboxTarget,
        turn: u64,
    ) -> Result<Claimed, InboxError> {
        let steps = self.queue(InboxTarget::NextStep).len() as i64;
        let mut claimed = self
            .mutate(log, InboxTarget::NextStep, 0, steps, Vec::new(), false)?
            .removed;
        if target == InboxTarget::NextTurn {
            claimed.extend(
                self.mutate(log, InboxTarget::NextTurn, 0, 1, Vec::new(), false)?
                    .removed,
            );
        }
        let notifications = claimed
            .iter()
            .map(|message| InboxNotification::Claimed {
                message: message.clone(),
                turn,
            })
            .collect();
        Ok(Claimed {
            messages: claimed,
            notifications,
        })
    }

    /// Apply splice coordinates to a queue and record the normalized result.
    ///
    /// `start` and `delete_count` are signed because the surface is a splice:
    /// a negative start counts from the end, and refusing one would make a
    /// caller compute the length itself against a queue it does not own.
    pub fn splice(
        &mut self,
        log: &dyn SessionLog,
        target: InboxTarget,
        start: i64,
        delete_count: i64,
        inserted: Vec<PendingMessage>,
    ) -> Result<Spliced, InboxError> {
        self.mutate(log, target, start, delete_count, inserted, true)
    }

    /// Where one id is pending, searching `next-turn` before `next-step`.
    fn locate(&self, id: &str) -> Option<(InboxTarget, usize)> {
        for target in [InboxTarget::NextTurn, InboxTarget::NextStep] {
            if let Some(index) = self.queue(target).iter().position(|m| m.id == id) {
                return Some((target, index));
            }
        }
        None
    }

    fn mutate(
        &mut self,
        log: &dyn SessionLog,
        target: InboxTarget,
        start: i64,
        delete_count: i64,
        inserted: Vec<PendingMessage>,
        cancels: bool,
    ) -> Result<Spliced, InboxError> {
        let len = self.queue(target).len();
        let start = normalize_start(start, len);
        let removed_count = normalize_delete(delete_count, len - start);
        // Nothing removed and nothing inserted is not a mutation: writing it
        // would put a record on the journal that a replay cannot distinguish
        // from a bug, and publish notifications about nothing.
        if removed_count == 0 && inserted.is_empty() {
            return Ok(Spliced::default());
        }
        let splice = InboxSplice {
            target,
            start,
            removed_count: (removed_count > 0).then_some(removed_count),
            inserted,
            outcome: (cancels && removed_count > 0).then_some(SpliceOutcome::Canceled),
        };
        self.check(&splice)?;

        // Durable first. A journal that refuses leaves the queues untouched,
        // because a message served from memory that no restart remembers is
        // worse than one that was never accepted.
        log.append(topic::INBOX_SPLICED, json!(splice))?;

        let removed = self.apply(&splice);
        let mut notifications = Vec::new();
        if cancels {
            notifications.extend(removed.iter().cloned().map(InboxNotification::Discarded));
        }
        notifications.extend(
            splice
                .inserted
                .iter()
                .cloned()
                .map(InboxNotification::Inserted),
        );
        Ok(Spliced {
            removed,
            notifications,
        })
    }

    /// Whether one normalized splice applies to the queues as they stand.
    fn check(&self, splice: &InboxSplice) -> Result<(), InboxError> {
        let queue = self.queue(splice.target);
        let removed = splice.removed();
        if splice.start > queue.len() || splice.start + removed > queue.len() {
            return Err(InboxError::OutOfRange(format!(
                "start {} and removal {} do not fit a queue of {}",
                splice.start,
                removed,
                queue.len()
            )));
        }
        // Uniqueness is over both queues together, and over the queues *after*
        // the splice: replacing a message with itself is legal, and it is only
        // a duplicate if it survives beside another entry of the same name.
        let mut after: Vec<&str> = Vec::with_capacity(queue.len() + splice.inserted.len());
        after.extend(queue[..splice.start].iter().map(|m| m.id.as_str()));
        after.extend(splice.inserted.iter().map(|m| m.id.as_str()));
        after.extend(
            queue[splice.start + removed..]
                .iter()
                .map(|m| m.id.as_str()),
        );
        let other = match splice.target {
            InboxTarget::NextTurn => &self.next_step,
            InboxTarget::NextStep => &self.next_turn,
        };
        after.extend(other.iter().map(|m| m.id.as_str()));
        let mut seen = std::collections::BTreeSet::new();
        for id in after {
            if !seen.insert(id) {
                return Err(InboxError::AlreadyPending(id.to_owned()));
            }
        }
        Ok(())
    }

    /// Apply one checked, normalized splice.
    fn apply(&mut self, splice: &InboxSplice) -> Vec<PendingMessage> {
        let removed = splice.removed();
        let queue = self.queue_mut(splice.target);
        queue
            .splice(
                splice.start..splice.start + removed,
                splice.inserted.iter().cloned(),
            )
            .collect()
    }
}

/// What one splice did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Spliced {
    pub removed: Vec<PendingMessage>,
    pub notifications: Vec<InboxNotification>,
}

/// What one boundary claimed.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Claimed {
    /// `next-step` input first, then the queued turn when one was claimed.
    pub messages: Vec<PendingMessage>,
    pub notifications: Vec<InboxNotification>,
}

/// A negative start counts back from the end; anything past either end lands
/// on it. Saturating, because a caller that computed `i64::MIN` still means
/// "the front".
fn normalize_start(start: i64, len: usize) -> usize {
    if start < 0 {
        let back = start.unsigned_abs() as usize;
        len.saturating_sub(back)
    } else {
        (start as usize).min(len)
    }
}

/// A removal stops at the end of the queue, and a negative one removes
/// nothing.
fn normalize_delete(delete_count: i64, available: usize) -> usize {
    if delete_count <= 0 {
        0
    } else {
        (delete_count as usize).min(available)
    }
}
