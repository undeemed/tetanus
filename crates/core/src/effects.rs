#[derive(Debug, thiserror::Error)]
pub enum EffectError {
    #[error("effect failed: {0}")]
    Failed(String),
}

/// RAII effect handle: registering returns a handle; dropping it
/// unwinds the registration (harness parity: "RAII effect handles;
/// every registration returns an EffectHandle; drop = unwind").
pub struct EffectHandle {
    undo: Option<Box<dyn FnOnce() + Send>>,
}

impl EffectHandle {
    pub fn new(undo: impl FnOnce() + Send + 'static) -> Self {
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
