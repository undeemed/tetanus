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
