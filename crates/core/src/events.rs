use tokio::sync::broadcast;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Event {
    pub topic: String,
    pub payload: serde_json::Value,
}

/// Typed broadcast bus; subscribers get replay-free live events.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }
    pub fn publish(&self, ev: Event) { let _ = self.tx.send(ev); }
    pub fn subscribe(&self) -> broadcast::Receiver<Event> { self.tx.subscribe() }
}
