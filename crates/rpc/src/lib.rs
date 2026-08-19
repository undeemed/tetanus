//! The JSON-RPC codec: one frame in, one frame out, over
//! [`tetanus_protocol::methods::Engine`].
//!
//! This crate is where the wire meets the contract, and it is deliberately the
//! only place that knows the two are different things. It parses a frame,
//! finds the call, hands the typed params to the engine, and writes the answer
//! back. It runs no turn, opens no journal and holds no session.
//!
//! Contract section 4.2's method table is wired call by call. This build
//! carries the handshake.
//!
//! It owns no transport either. A carrier reads bytes, calls [`Codec::frame`]
//! and writes what comes back, so the stdio carrier and the WebSocket carrier
//! differ only in how they move a string.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use tetanus_protocol::methods::{method, Engine};
use tetanus_protocol::rpc::{ErrorCode, Id, Message, Payload, Request, Response, RpcError, V2};

/// One connection's codec.
///
/// Per connection, not per process: the handshake is connection state.
/// Section 4.4.1 of the contract says `rpc.hello` is the first call on a
/// connection, and this is what remembers whether it has happened.
pub struct Codec {
    engine: Arc<dyn Engine>,
    greeted: AtomicBool,
}

impl Codec {
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        Self {
            engine,
            greeted: AtomicBool::new(false),
        }
    }

    /// Handle one frame and answer it.
    ///
    /// `None` means "write nothing", which is the right answer to a frame that
    /// asked no question: a notification, or a response to a request this
    /// server made. Every other frame is answered, malformed ones included,
    /// because a client that is waiting has to be released.
    pub async fn frame(&self, raw: &str) -> Option<String> {
        let response = self.answer(raw).await?;
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

    async fn answer(&self, raw: &str) -> Option<Response> {
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
            Ok(Message::Request(request)) => Some(self.dispatch(request).await),
            Err(error) => Some(refusal(ErrorCode::InvalidRequest, error.to_string())),
        }
    }

    async fn dispatch(&self, request: Request) -> Response {
        let id = request.id.clone();
        match self.call(request).await {
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

    async fn call(&self, request: Request) -> Result<serde_json::Value, RpcError> {
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
            unknown => Err(RpcError::new(
                ErrorCode::MethodNotFound,
                format!("no method `{unknown}`"),
            )
            .with_data(serde_json::json!({ "method": unknown }))),
        }
    }
}

/// The answer to a frame whose id could not be read. Section 4.1: it is still
/// answered, with `id: null`, because a client that is waiting has to be
/// released.
fn refusal(code: ErrorCode, message: impl Into<String>) -> Response {
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
