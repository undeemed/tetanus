//! What a subscription delivers, and how a caller reads it.

use std::sync::Arc;

use tetanus_protocol::methods::{AgentStatusPush, EventSink, SessionEventPush};
use tetanus_protocol::types::SessionEvent;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// One frame a subscription delivered, in the order the engine pushed it.
///
/// The two push kinds arrive interleaved on one stream rather than on two,
/// because their *relative* order is the fact a caller most often needs: a
/// surface renders "running" before the first event of the turn, and two
/// streams could not say that happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Event(SessionEventPush),
    Status(AgentStatusPush),
}

impl Update {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Event(push) => &push.session_id,
            Self::Status(push) => &push.session_id,
        }
    }

    /// The journal event, for a caller that only wants those.
    pub fn event(&self) -> Option<&SessionEvent> {
        match self {
            Self::Event(push) => Some(&push.event),
            Self::Status(_) => None,
        }
    }
}

/// The sink half: what the engine pushes into.
///
/// Unbounded for the reason the stdio carrier's queue is unbounded: the
/// alternative is either blocking the turn that is pushing or dropping an
/// event, and the session log *is* the stream, so a dropped event is a hole
/// in it.
pub(crate) struct Channel(pub(crate) UnboundedSender<Update>);

impl EventSink for Channel {
    fn session_event(&self, push: SessionEventPush) {
        // A closed receiver means the caller stopped reading. That is the
        // caller's decision, not a failure of the turn that pushed.
        let _ = self.0.send(Update::Event(push));
    }

    fn agent_status(&self, push: AgentStatusPush) {
        let _ = self.0.send(Update::Status(push));
    }
}

/// A live subscription's reading end.
///
/// Dropping this stops the caller reading; it does not close the subscription
/// on the engine, because closing is an async call and a `Drop` cannot make
/// one. [`Client::close`](crate::Client::close) closes everything a client
/// opened, which is the same promise a carrier makes when its peer hangs up,
/// and [`Subscription::close`] closes one on demand.
pub struct Subscription {
    pub(crate) subscription_id: String,
    /// The seq the subscription starts after; `-1` for an empty log.
    pub(crate) last_seq: i64,
    pub(crate) updates: UnboundedReceiver<Update>,
    pub(crate) client: Arc<crate::client::Inner>,
}

impl Subscription {
    pub fn id(&self) -> &str {
        &self.subscription_id
    }

    /// Seq of the last event this subscription starts *after*. Every higher
    /// seq arrives as an update.
    pub fn last_seq(&self) -> i64 {
        self.last_seq
    }

    /// The next update, waiting for one. `None` once the engine has dropped
    /// every sender, which happens when the subscription is closed.
    pub async fn next(&mut self) -> Option<Update> {
        self.updates.recv().await
    }

    /// The next update if one is already queued. Never waits.
    ///
    /// This is what makes collection deterministic: the engine pushes on the
    /// thread that appends, so by the time a call that ran a turn has
    /// returned, everything that turn produced is already in this queue.
    pub fn try_next(&mut self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    /// Everything already queued, in order.
    pub fn drain(&mut self) -> Vec<Update> {
        let mut updates = Vec::new();
        while let Some(update) = self.try_next() {
            updates.push(update);
        }
        updates
    }

    /// Close this subscription on the engine and stop delivery.
    pub async fn close(self) -> Result<(), crate::SdkError> {
        self.client.unsubscribe(&self.subscription_id).await
    }
}
