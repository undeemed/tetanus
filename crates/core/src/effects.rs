//! Reversible effects: a registration hands back a handle, and unwinding is
//! dropping it.

use std::panic::AssertUnwindSafe;

#[derive(Debug, thiserror::Error)]
pub enum EffectError {
    #[error("effect failed: {0}")]
    Failed(String),
}

/// RAII effect handle: registering returns a handle; dropping it
/// unwinds the registration (harness parity: "RAII effect handles;
/// every registration returns an EffectHandle; drop = unwind").
/// The undo is `Sync` as well as `Send` so that a handle may live inside
/// shared state: a registration held by an `Arc`-shared owner is the normal
/// case, and a handle that made its owner non-`Sync` would be a trap.
pub struct EffectHandle {
    undo: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl EffectHandle {
    pub fn new(undo: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            undo: Some(Box::new(undo)),
        }
    }
    /// Keep the effect permanently (leak the undo).
    pub fn forget(mut self) {
        self.undo = None;
    }
}

impl Drop for EffectHandle {
    fn drop(&mut self) {
        if let Some(u) = self.undo.take() {
            u();
        }
    }
}

/// Several effects with one lifetime, unwound newest first.
///
/// Reverse order is not a preference. An effect registered second may stand on
/// the first - a listener on a service, a subscription on a session - so
/// undoing the first while the second is still live is the one order in which
/// something can observe a half-torn world.
///
/// Parity: upstream composes teardown from a generator that yields disposers
/// and runs the yields backwards, with a nested scope's own disposer taking its
/// place in that order (`packages/core/scope/tests/scope.spec.ts`, "exposes the
/// exact raw disposer for ordered composite teardown"). [`EffectScope::keep`]
/// is the yield, and [`EffectScope::into_handle`] is the nested disposer.
#[derive(Default)]
pub struct EffectScope {
    effects: Vec<EffectHandle>,
}

impl EffectScope {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an undo to run when the scope unwinds.
    pub fn on_unwind(&mut self, undo: impl FnOnce() + Send + Sync + 'static) {
        self.keep(EffectHandle::new(undo));
    }

    /// Hold a handle some other registration already returned, so it unwinds
    /// with the scope instead of at the end of the statement that made it.
    pub fn keep(&mut self, effect: EffectHandle) {
        self.effects.push(effect);
    }

    /// How many effects are still to unwind.
    pub fn len(&self) -> usize {
        self.effects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    /// Unwind now, newest effect first, leaving the scope empty.
    ///
    /// Idempotent, because a scope that has unwound holds nothing: a second
    /// call, or the drop that follows one, undoes nothing twice.
    ///
    /// Every effect runs even when one of them panics. The panics are returned
    /// rather than propagated, since the effects still waiting behind a bad one
    /// are exactly the ones that would otherwise leak.
    pub fn unwind(&mut self) -> Vec<EffectError> {
        let mut faults = Vec::new();
        while let Some(effect) = self.effects.pop() {
            if let Err(panic) = std::panic::catch_unwind(AssertUnwindSafe(move || drop(effect))) {
                faults.push(EffectError::Failed(panicked(&panic)));
            }
        }
        faults
    }

    /// Keep every effect permanently: nothing unwinds, now or on drop.
    pub fn forget(mut self) {
        for effect in std::mem::take(&mut self.effects) {
            effect.forget();
        }
    }

    /// Collapse the scope into one handle, so it can nest: an enclosing scope
    /// that keeps this handle unwinds the whole inner scope at the inner
    /// scope's place in the outer order.
    pub fn into_handle(mut self) -> EffectHandle {
        let mut inner = EffectScope {
            effects: std::mem::take(&mut self.effects),
        };
        EffectHandle::new(move || {
            let _ = inner.unwind();
        })
    }
}

impl Drop for EffectScope {
    fn drop(&mut self) {
        for fault in self.unwind() {
            tracing::error!(%fault, "an undo panicked while a scope unwound");
        }
    }
}

/// What a caught panic was about, as far as the payload says.
fn panicked(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "the undo panicked".to_string()
}
