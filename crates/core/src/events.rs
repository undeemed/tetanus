//! Typed event bus carrying the four dispatch modes dsh documents for plugin
//! authors (upstream `docs/cordis-primer.md`, "Dispatch Modes"):
//!
//! | Mode        | Awaited | Order                      | Returns a value |
//! |-------------|---------|----------------------------|-----------------|
//! | `emit`      | no      | registration order         | no              |
//! | `waterfall` | no*     | registration order         | yes             |
//! | `parallel`  | yes     | all listeners concurrently | no              |
//! | `serial`    | yes     | registration order, bails  | yes             |
//!
//! \* upstream `waterfall` is unawaited because its return value may itself be
//! a promise. Rust has no such value, so a waterfall dispatch is an `async fn`
//! whose composed chain is awaited by the caller. The semantics that matter -
//! around-middleware, `next()` delegation, short-circuit veto - are preserved.
//!
//! The dispatch mode is part of an event's public contract: it is a `const` on
//! the [`Event`] impl, and registering or dispatching through the wrong mode
//! panics instead of silently doing nothing.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::effects::EffectHandle;

/// A boxed future borrowing its event payload for `'a`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMode {
    Emit,
    Waterfall,
    Parallel,
    Serial,
}

/// One event declaration: its wire topic, its dispatch mode, and the value the
/// mode returns (`()` for the two modes that do not return one).
pub trait Event: Send + Sync + 'static {
    const TOPIC: &'static str;
    const MODE: DispatchMode;
    type Output: Send + 'static;
}

pub type EmitFn<E> = Arc<dyn Fn(&E) + Send + Sync>;
pub type ParallelFn<E> = Arc<dyn Fn(&E) -> BoxFuture<'_, ()> + Send + Sync>;
pub type SerialFn<E> = Arc<dyn Fn(&E) -> BoxFuture<'_, Option<<E as Event>::Output>> + Send + Sync>;
pub type WaterfallFn<E> =
    Arc<dyn Fn(&mut E, Next<E>) -> BoxFuture<'_, <E as Event>::Output> + Send + Sync>;

/// The built-in behavior a waterfall chain wraps. It runs when every listener
/// delegated through [`Next::run`], and never runs if one of them vetoed.
pub type Terminal<E> = Arc<dyn Fn(&mut E) -> BoxFuture<'_, <E as Event>::Output> + Send + Sync>;

/// The continuation handed to a waterfall listener. Call [`Next::run`] to
/// delegate to the next listener (finally the terminal); drop it and return a
/// value of your own to short-circuit the rest of the chain.
pub struct Next<E: Event> {
    chain: Arc<Vec<WaterfallFn<E>>>,
    idx: usize,
    terminal: Terminal<E>,
}

impl<E: Event> Next<E> {
    pub async fn run(self, ev: &mut E) -> E::Output {
        match self.chain.get(self.idx) {
            Some(listener) => {
                let listener = Arc::clone(listener);
                let next = Next {
                    chain: Arc::clone(&self.chain),
                    idx: self.idx + 1,
                    terminal: Arc::clone(&self.terminal),
                };
                listener(ev, next).await
            }
            None => (self.terminal)(ev).await,
        }
    }
}

struct Inner {
    next_id: AtomicU64,
    slots: Mutex<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

/// Shared, cheaply cloned handle to one bus. Listener registrations are
/// reversible effects: the returned [`EffectHandle`] removes the listener when
/// dropped, so a component's teardown unwinds its own wiring.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Inner>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                next_id: AtomicU64::new(0),
                slots: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn on_emit<E: Event>(&self, f: impl Fn(&E) + Send + Sync + 'static) -> EffectHandle {
        self.register::<E, EmitFn<E>>(DispatchMode::Emit, Arc::new(f))
    }

    pub fn on_parallel<E, F>(&self, f: F) -> EffectHandle
    where
        E: Event,
        F: Fn(&E) -> BoxFuture<'_, ()> + Send + Sync + 'static,
    {
        self.register::<E, ParallelFn<E>>(DispatchMode::Parallel, Arc::new(f))
    }

    pub fn on_serial<E, F>(&self, f: F) -> EffectHandle
    where
        E: Event,
        F: Fn(&E) -> BoxFuture<'_, Option<E::Output>> + Send + Sync + 'static,
    {
        self.register::<E, SerialFn<E>>(DispatchMode::Serial, Arc::new(f))
    }

    pub fn on_waterfall<E, F>(&self, f: F) -> EffectHandle
    where
        E: Event,
        F: Fn(&mut E, Next<E>) -> BoxFuture<'_, E::Output> + Send + Sync + 'static,
    {
        self.register::<E, WaterfallFn<E>>(DispatchMode::Waterfall, Arc::new(f))
    }

    /// Run listeners in registration order, ignoring their return values.
    pub fn emit<E: Event>(&self, ev: &E) {
        for listener in self.snapshot::<E, EmitFn<E>>(DispatchMode::Emit) {
            listener(ev);
        }
    }

    /// Run every listener concurrently and resolve once all have settled.
    pub async fn parallel<E: Event>(&self, ev: &E) {
        let listeners = self.snapshot::<E, ParallelFn<E>>(DispatchMode::Parallel);
        futures_util::future::join_all(listeners.iter().map(|l| l(ev))).await;
    }

    /// Await listeners in registration order until one bails; that bail value
    /// is the result.
    pub async fn serial<E: Event>(&self, ev: &E) -> Option<E::Output> {
        for listener in self.snapshot::<E, SerialFn<E>>(DispatchMode::Serial) {
            if let Some(bail) = listener(ev).await {
                return Some(bail);
            }
        }
        None
    }

    /// Compose listeners as around-middleware over `terminal`, the built-in
    /// behavior of the dispatching component.
    pub async fn waterfall<E: Event>(&self, ev: &mut E, terminal: Terminal<E>) -> E::Output {
        let chain = Arc::new(self.snapshot::<E, WaterfallFn<E>>(DispatchMode::Waterfall));
        Next {
            chain,
            idx: 0,
            terminal,
        }
        .run(ev)
        .await
    }

    /// Number of listeners registered for `E`; used by tests and diagnostics.
    pub fn listener_count<E: Event>(&self) -> usize {
        let slots = self.inner.slots.lock().expect("bus lock");
        match slots.get(&TypeId::of::<E>()) {
            Some(slot) => match E::MODE {
                DispatchMode::Emit => len_of::<EmitFn<E>>(slot.as_ref()),
                DispatchMode::Parallel => len_of::<ParallelFn<E>>(slot.as_ref()),
                DispatchMode::Serial => len_of::<SerialFn<E>>(slot.as_ref()),
                DispatchMode::Waterfall => len_of::<WaterfallFn<E>>(slot.as_ref()),
            },
            None => 0,
        }
    }

    fn register<E: Event, L: Clone + Send + Sync + 'static>(
        &self,
        mode: DispatchMode,
        listener: L,
    ) -> EffectHandle {
        assert_mode::<E>(mode);
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut slots = self.inner.slots.lock().expect("bus lock");
            let slot = slots
                .entry(TypeId::of::<E>())
                .or_insert_with(|| Box::new(Vec::<(u64, L)>::new()));
            slot.downcast_mut::<Vec<(u64, L)>>()
                .expect("one listener type per event, fixed by its dispatch mode")
                .push((id, listener));
        }
        let inner = Arc::clone(&self.inner);
        let key = TypeId::of::<E>();
        EffectHandle::new(move || {
            if let Some(slot) = inner.slots.lock().expect("bus lock").get_mut(&key) {
                if let Some(list) = slot.downcast_mut::<Vec<(u64, L)>>() {
                    list.retain(|(other, _)| *other != id);
                }
            }
        })
    }

    fn snapshot<E: Event, L: Clone + Send + Sync + 'static>(&self, mode: DispatchMode) -> Vec<L> {
        assert_mode::<E>(mode);
        self.inner
            .slots
            .lock()
            .expect("bus lock")
            .get(&TypeId::of::<E>())
            .and_then(|slot| slot.downcast_ref::<Vec<(u64, L)>>())
            .map(|list| list.iter().map(|(_, l)| l.clone()).collect())
            .unwrap_or_default()
    }
}

fn assert_mode<E: Event>(used: DispatchMode) {
    assert_eq!(
        E::MODE,
        used,
        "event `{}` is declared {:?} and can only be used through that mode",
        E::TOPIC,
        E::MODE
    );
}

fn len_of<L: 'static>(slot: &(dyn Any + Send + Sync)) -> usize {
    slot.downcast_ref::<Vec<(u64, L)>>().map_or(0, Vec::len)
}
