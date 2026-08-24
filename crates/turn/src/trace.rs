//! Read the ordered event sequence of a turn without changing it.
//!
//! The driver in [`crate::engine`] owns the documented order. This module only
//! listens: one listener per documented extension point, each waterfall
//! listener delegating straight through `next()`. A headless run prints the
//! result; the conformance suite asserts it.

use std::sync::{Arc, Mutex};

use tetanus_core::{EffectHandle, Event, EventBus};
use tetanus_session::SessionEventDispatch;

use crate::events::{
    AgentRequest, AssemblePrompt, LlmStream, PreStep, RequestError, ToolsExecute, ToolsPermission,
    ToolsPostExecute, ToolsPreExecute, TurnStopping,
};

/// One observed event. Durable events carry their journal sequence number and
/// payload; the in-memory extension points carry only their topic.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceEntry {
    pub topic: String,
    pub seq: Option<u64>,
    pub data: Option<serde_json::Value>,
}

/// A live trace. Listeners stay registered until this is dropped.
pub struct TurnTrace {
    entries: Arc<Mutex<Vec<TraceEntry>>>,
    _handles: Vec<EffectHandle>,
}

impl TurnTrace {
    /// Attach to every documented event of a turn.
    pub fn attach(bus: &EventBus) -> Self {
        let entries: Arc<Mutex<Vec<TraceEntry>>> = Arc::default();

        macro_rules! watch_waterfall {
            ($event:ty, $topic:literal) => {{
                let entries = entries.clone();
                bus.on_waterfall::<$event, _>(move |ev, next| {
                    let entries = entries.clone();
                    Box::pin(async move {
                        push(
                            &entries,
                            TraceEntry {
                                topic: $topic.into(),
                                seq: None,
                                data: None,
                            },
                        );
                        next.run(ev).await
                    })
                })
            }};
        }

        let durable = {
            let entries = entries.clone();
            bus.on_emit::<SessionEventDispatch>(move |ev| {
                push(
                    &entries,
                    TraceEntry {
                        topic: ev.event.ty.clone(),
                        seq: Some(ev.event.seq),
                        data: Some(ev.event.data.clone()),
                    },
                );
            })
        };
        let stopping = {
            let entries = entries.clone();
            bus.on_serial::<TurnStopping, _>(move |_ev| {
                let entries = entries.clone();
                Box::pin(async move {
                    push(
                        &entries,
                        TraceEntry {
                            topic: TurnStopping::TOPIC.into(),
                            seq: None,
                            data: None,
                        },
                    );
                    None
                })
            })
        };

        let handles = vec![
            durable,
            watch_waterfall!(PreStep, "agent/pre-step"),
            watch_waterfall!(AssemblePrompt, "system-prompt/assemble"),
            watch_waterfall!(AgentRequest, "agent/request"),
            watch_waterfall!(LlmStream, "llm/stream"),
            watch_waterfall!(RequestError, "agent/request-error"),
            watch_waterfall!(ToolsPermission, "tools/permission"),
            watch_waterfall!(ToolsPreExecute, "tools/pre-execute"),
            watch_waterfall!(ToolsExecute, "tools/execute"),
            watch_waterfall!(ToolsPostExecute, "tools/post-execute"),
            stopping,
        ];

        Self {
            entries,
            _handles: handles,
        }
    }

    pub fn entries(&self) -> Vec<TraceEntry> {
        self.entries.lock().expect("trace").clone()
    }

    /// The observed sequence as bare topics, in order.
    pub fn topics(&self) -> Vec<String> {
        self.entries().into_iter().map(|e| e.topic).collect()
    }
}

fn push(entries: &Arc<Mutex<Vec<TraceEntry>>>, entry: TraceEntry) {
    entries.lock().expect("trace").push(entry);
}
