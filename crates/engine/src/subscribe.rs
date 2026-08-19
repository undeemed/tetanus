//! The push hub: where a subscription's events go.
//!
//! One session's log publishes every append on its own bus. A subscription
//! forwards those to an `EventSink`, which the carrier supplies: the RPC
//! carriers write a frame, the in-process caller hands to a renderer. Neither
//! the engine nor a renderer has to know which it is talking to.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use std::collections::BTreeMap;
use tetanus_core::EffectHandle;
use tetanus_protocol::methods::{
    Ack, EventSink, SessionEventPush, SessionSubscribeResult, SessionUnsubscribeParams,
};
use tetanus_protocol::rpc::RpcError;
use tetanus_session::{SessionEvent, SessionEventDispatch, SessionLog};

use crate::convert::session_event;
use crate::session::LiveSession;

/// A subscription lives until it is closed. Dropping the handle unregisters
/// the bus listener, which is the whole of the cleanup.
struct Subscription {
    _listener: EffectHandle,
}

/// The state a listener needs while a replay is still running.
///
/// Registering the listener first and replaying second would deliver an event
/// appended in between twice; replaying first and registering second would
/// lose it. So the listener buffers until the replay says how far it got, and
/// then only the events past that point are released.
#[derive(Default)]
struct Gate {
    open: bool,
    delivered_through: u64,
    held: Vec<SessionEvent>,
}

pub struct Hub {
    live: Mutex<BTreeMap<String, Subscription>>,
    counter: AtomicU64,
}

impl Hub {
    pub fn new() -> Self {
        Self {
            live: Mutex::new(BTreeMap::new()),
            counter: AtomicU64::new(0),
        }
    }

    /// Start pushing one session's events to a sink.
    ///
    /// `from_seq` replays the journal from that seq before live delivery
    /// starts; omitting it delivers live events only. Either way `last_seq` is
    /// the seq the subscription starts after, so every higher seq arrives as a
    /// push and the caller needs no second call to find the boundary.
    pub fn subscribe(
        &self,
        session: &LiveSession,
        from_seq: Option<u64>,
        sink: Arc<dyn EventSink>,
    ) -> SessionSubscribeResult {
        let session_id = session.header.session_id.clone();
        let gate = Arc::new(Mutex::new(Gate::default()));

        let listener = {
            let gate = Arc::clone(&gate);
            let sink = Arc::clone(&sink);
            let session_id = session_id.clone();
            session
                .bus
                .on_emit::<SessionEventDispatch>(move |dispatch| {
                    let mut gate = gate.lock().expect("gate");
                    if !gate.open {
                        gate.held.push(dispatch.event.clone());
                        return;
                    }
                    if dispatch.event.seq <= gate.delivered_through {
                        return;
                    }
                    gate.delivered_through = dispatch.event.seq;
                    sink.session_event(push(&session_id, dispatch.event.clone()));
                })
        };

        let known = session.log.events();
        let last_seq = known.len() as i64 - 1;
        if let Some(from) = from_seq {
            for event in known.into_iter().skip(from as usize) {
                sink.session_event(push(&session_id, event));
            }
        }

        // Release whatever arrived while the replay was running, once. The
        // gate stays locked across the flush so a live event cannot overtake a
        // held one; a sink must therefore not append to this same log from
        // inside its callback, which would deadlock on the log itself anyway.
        let mut state = gate.lock().expect("gate");
        state.delivered_through = last_seq.max(0) as u64;
        state.open = true;
        for event in std::mem::take(&mut state.held) {
            if last_seq >= 0 && event.seq <= last_seq as u64 {
                continue;
            }
            state.delivered_through = event.seq;
            sink.session_event(push(&session_id, event));
        }
        drop(state);

        let subscription_id = format!("sub-{}", self.counter.fetch_add(1, Ordering::Relaxed));
        self.live.lock().expect("subscriptions").insert(
            subscription_id.clone(),
            Subscription {
                _listener: listener,
            },
        );

        SessionSubscribeResult {
            subscription_id,
            last_seq,
        }
    }

    /// Close one subscription. Closing an id that is already gone is not an
    /// error, because two closers racing each other is not a fault; the `ok`
    /// flag says which of them did the work.
    pub fn unsubscribe(&self, params: SessionUnsubscribeParams) -> Result<Ack, RpcError> {
        let removed = self
            .live
            .lock()
            .expect("subscriptions")
            .remove(&params.subscription_id)
            .is_some();
        Ok(Ack { ok: removed })
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

fn push(session_id: &str, event: SessionEvent) -> SessionEventPush {
    SessionEventPush {
        session_id: session_id.to_string(),
        event: session_event(event),
    }
}
