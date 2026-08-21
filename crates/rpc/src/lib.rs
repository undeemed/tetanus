//! The JSON-RPC codec: one frame in, one frame out, over
//! [`tetanus_protocol::methods::Engine`].
//!
//! This crate is where the wire meets the contract, and it is deliberately the
//! only place that knows the two are different things. It parses a frame,
//! finds the call, hands the typed params to the engine, and writes the answer
//! back. It runs no turn, opens no journal and holds no session.
//!
//! It owns no transport either. A carrier reads bytes, calls [`Codec::frame`]
//! and writes what comes back, so the stdio carrier and the WebSocket carrier
//! differ only in how they move a string.

// Debug prints escaped into a release once: the stdio carrier wrote `DBG`
// lines to stderr for every frame it moved. `-D warnings` did not catch them,
// because both lints are allow-by-default. A carrier reports through its own
// output or not at all, so denying them here is the guard.
#![deny(clippy::print_stderr, clippy::print_stdout)]

pub mod stdio;
pub mod websocket;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use tetanus_protocol::methods::{
    method, push, AgentStatusPush, Engine, EventSink, SessionEventPush, SessionUnsubscribeParams,
};
use tetanus_protocol::rpc::{
    ErrorCode, Id, Message, Notification, Payload, Request, Response, RpcError, V2,
};
use tokio::sync::mpsc::UnboundedSender;

/// One connection's codec.
///
/// Per connection, not per process: the handshake is connection state.
/// Section 4.4.1 of the contract says `rpc.hello` is the first call on a
/// connection, and this is what remembers whether it has happened.
pub struct Codec {
    engine: Arc<dyn Engine>,
    greeted: AtomicBool,
    /// The subscriptions this connection opened and has not closed.
    open: Mutex<Vec<String>>,
}

impl Codec {
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        Self {
            engine,
            greeted: AtomicBool::new(false),
            open: Mutex::new(Vec::new()),
        }
    }

    /// Close what this connection left open.
    ///
    /// A carrier calls this when the peer is gone. A subscription outlives the
    /// call that made it, so nothing else would ever end one, and the engine
    /// would keep pushing into a sink whose socket is shut. Failures are
    /// dropped on purpose: there is no longer anyone to report them to.
    pub async fn close(&self) {
        let open = std::mem::take(&mut *self.open.lock().expect("open"));
        for subscription_id in open {
            let _ = self
                .engine
                .session_unsubscribe(SessionUnsubscribeParams { subscription_id })
                .await;
        }
    }

    /// Handle one frame and answer it.
    ///
    /// `None` means "write nothing", which is the right answer to a frame that
    /// asked no question: a notification, or a response to a request this
    /// server made. Every other frame is answered, malformed ones included,
    /// because a client that is waiting has to be released.
    ///
    /// `sink` is where this connection wants its pushes. It is an argument
    /// rather than a field so a carrier may route a subscription somewhere
    /// other than the connection that asked for it.
    pub async fn frame(&self, raw: &str, sink: Arc<dyn EventSink>) -> Option<String> {
        let response = self.answer(raw, sink).await?;
        // A response that cannot be serialized is a bug in this crate, not in
        // the frame, so it is reported as one rather than dropped.
        Some(serde_json::to_string(&response).unwrap_or_else(|error| {
            let internal = Response {
                jsonrpc: V2,
                id: response.id.clone(),
                payload: Payload::Error(RpcError::new(ErrorCode::Internal, error.to_string())),
            };
            serde_json::to_string(&internal).expect("an error object always serializes")
        }))
    }

    async fn answer(&self, raw: &str, sink: Arc<dyn EventSink>) -> Option<Response> {
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(error) => return Some(refusal(ErrorCode::ParseError, error.to_string())),
        };
        if value.is_array() {
            return Some(refusal(
                ErrorCode::InvalidRequest,
                "batch arrays are not part of contract 1.0",
            ));
        }

        match serde_json::from_value::<Message>(value) {
            // A notification is one-way. Contract 1.0 defines none from a
            // client, and an unknown one is ignored rather than refused, so an
            // engine may add one in a minor version.
            Ok(Message::Notification(_)) => None,
            // A response answers a request this server made. It is not a
            // question, so it gets no answer.
            Ok(Message::Response(_)) => None,
            Ok(Message::Request(request)) => Some(self.dispatch(request, sink).await),
            Err(error) => Some(refusal(ErrorCode::InvalidRequest, error.to_string())),
        }
    }

    async fn dispatch(&self, request: Request, sink: Arc<dyn EventSink>) -> Response {
        let id = request.id.clone();
        match self.call(request, sink).await {
            Ok(result) => Response {
                jsonrpc: V2,
                id,
                payload: Payload::Result(result),
            },
            Err(error) => Response {
                jsonrpc: V2,
                id,
                payload: Payload::Error(error),
            },
        }
    }

    async fn call(
        &self,
        request: Request,
        sink: Arc<dyn EventSink>,
    ) -> Result<serde_json::Value, RpcError> {
        let method = request.method.as_str();
        if method != method::HELLO && !self.greeted.load(Ordering::Acquire) {
            // Section 4.4.1. The handshake settles the protocol version, so a
            // call made before it has not agreed what it is speaking.
            return Err(RpcError::new(
                ErrorCode::InvalidRequest,
                format!("`{}` is the first call on a connection", method::HELLO),
            ));
        }

        let params = request.params.unwrap_or_else(|| serde_json::json!({}));
        let engine = &self.engine;
        match method {
            method::HELLO => {
                let result = encode(engine.hello(typed(params)?).await?)?;
                // The handshake is settled by the engine accepting it, not by
                // the frame arriving. A refused hello leaves the connection
                // ungreeted, so a client may correct itself and try again.
                self.greeted.store(true, Ordering::Release);
                Ok(result)
            }
            method::SESSION_CREATE => encode(engine.session_create(typed(params)?).await?),
            method::SESSION_LIST => encode(engine.session_list().await?),
            method::SESSION_EVENTS => encode(engine.session_events(typed(params)?).await?),
            // Routed while it is still reserved, so a caller meets the
            // `NotImplemented` section 4.2 promises for a reserved call rather
            // than the `MethodNotFound` it would get for a method no contract
            // names.
            method::SESSION_FORK => encode(engine.session_fork(typed(params)?).await?),
            method::SESSION_SUBSCRIBE => {
                let result = engine.session_subscribe(typed(params)?, sink).await?;
                self.open
                    .lock()
                    .expect("open")
                    .push(result.subscription_id.clone());
                encode(result)
            }
            method::SESSION_UNSUBSCRIBE => {
                let params: SessionUnsubscribeParams = typed(params)?;
                self.open
                    .lock()
                    .expect("open")
                    .retain(|open| *open != params.subscription_id);
                encode(engine.session_unsubscribe(params).await?)
            }
            method::AGENT_PROMPT => encode(engine.agent_prompt(typed(params)?).await?),
            method::AGENT_STATUS => encode(engine.agent_status(typed(params)?).await?),
            method::AGENT_INTERRUPT => encode(engine.agent_interrupt(typed(params)?).await?),
            method::CATALOG_TOOLS => encode(engine.catalog_tools().await?),
            method::CATALOG_MODELS => encode(engine.catalog_models().await?),
            method::CONFIG_DUMP => encode(engine.config_dump().await?),
            unknown => Err(RpcError::new(
                ErrorCode::MethodNotFound,
                format!("no method `{unknown}`"),
            )
            .with_data(serde_json::json!({ "method": unknown }))),
        }
    }
}

/// The connection's [`EventSink`]: serialize the push, write it as a frame.
///
/// Shared by both carriers on purpose: contract section 4.1 says the stdio and
/// WebSocket carriers implement [`EventSink`] as "serialize and write a
/// frame", and one implementation is how that stays true of both.
///
/// The `Option` is the writer's stop signal: `None` means "no more frames".
pub(crate) struct Frames(pub(crate) UnboundedSender<Option<String>>);

impl Frames {
    fn notify<T: serde::Serialize>(&self, method: &str, params: T) {
        let frame = Notification {
            jsonrpc: V2,
            method: method.to_string(),
            params: Some(serde_json::to_value(params).expect("a push serializes")),
        };
        // A send that fails means the peer is gone, which is not this side's
        // problem to report: the carrier is already on its way out.
        let _ = self.0.send(Some(
            serde_json::to_string(&frame).expect("a frame serializes"),
        ));
    }
}

impl EventSink for Frames {
    fn session_event(&self, event: SessionEventPush) {
        self.notify(push::SESSION_EVENT, event);
    }

    fn agent_status(&self, status: AgentStatusPush) {
        self.notify(push::AGENT_STATUS, status);
    }
}

/// The answer to a frame whose id could not be read. Section 4.1: it is still
/// answered, with `id: null`, because a client that is waiting has to be
/// released.
pub(crate) fn refusal(code: ErrorCode, message: impl Into<String>) -> Response {
    Response {
        jsonrpc: V2,
        id: Id::Null,
        payload: Payload::Error(RpcError::new(code, message)),
    }
}

/// A call's params, or `InvalidParams` naming the field at fault.
///
/// An absent `params` arrived here as `{}`, so a call whose params are all
/// optional accepts both, and a call that needs a field reports that field
/// rather than reporting the absence of the object.
fn typed<T: DeserializeOwned>(params: serde_json::Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|error| {
        let message = error.to_string();
        let field = field_at_fault(&message).map(str::to_string);
        let error = RpcError::new(ErrorCode::InvalidParams, message);
        match field {
            Some(field) => error.with_data(serde_json::json!({ "field": field })),
            None => error,
        }
    })
}

/// The one field serde named, when it named one.
///
/// `data.field` is a promise the contract makes "when one field is at fault",
/// and serde says which in exactly those cases: a missing field, an unknown
/// one, or a value of the wrong type at a named key. A message that names none
/// gets no `field`, rather than a guess a surface would render as fact.
fn field_at_fault(message: &str) -> Option<&str> {
    let (_, rest) = message.split_once('`')?;
    let (field, _) = rest.split_once('`')?;
    (!field.is_empty()).then_some(field)
}

fn encode<T: serde::Serialize>(value: T) -> Result<serde_json::Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))
}
