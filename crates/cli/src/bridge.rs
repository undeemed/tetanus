//! The `/api` bridge: the published contract over plain HTTP.
//!
//! Upstream's `host/apiproxy` is the API gateway every client shape shares,
//! and the shape that matters here is its wire rule: a client POSTs
//! `/api/<method>` with the parameters as the body and reads the result back.
//! It is a second door onto one room - the same contract the WebSocket
//! carries, the same engine behind it - for a client that cannot hold a socket
//! open.
//!
//! # One dispatch table, not two
//!
//! The frame this builds goes through `crates/rpc`'s codec, the same one the
//! socket feeds. A bridge that matched method names for itself would be a
//! second table deciding what the contract answers, and the two would
//! disagree on the first method somebody added to one of them.
//!
//! That also settles the handshake question: the codec requires `rpc.hello`
//! first and this bridge holds one codec, so a caller greets the bridge once
//! and its later calls are answered. A bridge that minted a fresh codec per
//! POST would have to either skip the handshake - deciding, here, a contract
//! rule that is not this file's to decide - or demand a hello per request.
//!
//! # Why `application/json` is demanded before anything is dispatched
//!
//! Upstream: "Every `/api` POST must declare the `application/json` media
//! type - anything else is refused with 415 before dispatch, so cross-site
//! 'simple' requests (which browsers send without a CORS preflight) can never
//! execute a side-effectful method blind."
//!
//! That sentence is the whole security argument for this file. A form post
//! from any page on the internet reaches a loopback server without asking the
//! browser's permission, because three media types are simple enough to skip
//! the preflight. `application/json` is not one of them, so demanding it turns
//! every cross-site attempt into a preflight, which the origin rules then
//! refuse.
//!
//! # What an HTTP status means here, and what it does not
//!
//! The status is the carrier's: 415 for a media type, 400 for a body that is
//! not JSON, 405 for anything that is not a POST. A method that ran and failed
//! answers 200 with the error in the envelope, because that failure is the
//! engine's answer and not a fault of the transport - the same split upstream
//! draws when it says business errors ride the result and "HTTP status
//! expresses only the carrier".

use std::sync::Arc;

use tetanus_host::{Handler, Pattern, Registered, Request, Response, Status, Taken, WebServer};
use tetanus_protocol::methods::{AgentStatusPush, Engine, EventSink, SessionEventPush};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_rpc::Codec;

/// Where the bridge lives. A prefix, because the method is the rest of it.
pub const PREFIX: &str = "/api/";

/// The media type every call must declare, per the argument above.
const JSON: &str = "application/json";

/// Mount the bridge on a carrier.
pub fn mount(server: &WebServer, engine: Arc<dyn Engine>) -> Result<Registered, Taken> {
    let codec = Arc::new(Codec::new(engine));
    let handler: Handler = Arc::new(move |request| {
        let codec = Arc::clone(&codec);
        Box::pin(async move { answer(codec, request).await })
    });
    server.register(Pattern::Prefix, PREFIX, handler)
}

/// One call, from the request that carried it to the answer that goes back.
async fn answer(codec: Arc<Codec>, request: Request) -> Response {
    if request.method != "POST" {
        return refused(
            Status::MethodNotAllowed,
            ErrorCode::InvalidRequest,
            "a call is a POST",
        )
        .with("allow", "POST");
    }
    // Before dispatch, and before the body is even looked at.
    let declared = request
        .header("content-type")
        .and_then(|kind| kind.split(';').next())
        .unwrap_or_default()
        .trim()
        .to_string();
    if !declared.eq_ignore_ascii_case(JSON) {
        return refused(
            Status::UnsupportedMedia,
            ErrorCode::InvalidRequest,
            "a call declares application/json",
        );
    }
    let Some(method) = request
        .path
        .strip_prefix(PREFIX)
        .filter(|named| !named.is_empty())
    else {
        return refused(
            Status::NotFound,
            ErrorCode::MethodNotFound,
            "a call names a method",
        );
    };
    // An empty body is `{}`: a call with no parameters should not have to send
    // two characters to say so.
    let params: serde_json::Value = match request.body.is_empty() {
        true => serde_json::json!({}),
        false => match serde_json::from_slice(&request.body) {
            Ok(params) => params,
            Err(err) => {
                return refused(
                    Status::BadRequest,
                    ErrorCode::ParseError,
                    &format!("the body is not JSON: {err}"),
                )
            }
        },
    };

    // The id is the carrier's, not the caller's: one POST is one call, and the
    // answer goes back down the connection it came up. A caller that wants to
    // correlate has the response body in its hand.
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let raw = frame.to_string();
    match codec.frame(&raw, Arc::new(Unheard)).await {
        Some(answer) => Response::body(
            Status::Ok,
            "application/json; charset=utf-8",
            answer.into_bytes(),
        ),
        // Only a notification produces no answer, and nothing this bridge
        // builds is one. Saying so beats an empty 200 that reads as success.
        None => refused(
            Status::BadRequest,
            ErrorCode::InvalidRequest,
            "that call has no answer over this carrier",
        ),
    }
}

/// The sink for a carrier with nowhere to push.
///
/// `session.subscribe` over HTTP would have to hold the response open and
/// stream, which is upstream's SSE frame and is not this slice. Until then a
/// push here is dropped rather than buffered: a subscription whose events go
/// nowhere is a caller waiting for something that will never arrive, and
/// pretending to hold them would make that worse rather than better. The
/// socket at `/api/ws` is the carrier that pushes.
struct Unheard;

impl EventSink for Unheard {
    fn session_event(&self, _: SessionEventPush) {}
    fn agent_status(&self, _: AgentStatusPush) {}
}

/// A refusal by the carrier: a status that means it, and the same envelope
/// shape a caller reads on every other answer.
fn refused(status: Status, code: ErrorCode, said: &str) -> Response {
    let error = RpcError::new(code, said);
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": serde_json::Value::Null,
        "error": serde_json::to_value(&error).unwrap_or(serde_json::Value::Null),
    });
    Response::body(
        status,
        "application/json; charset=utf-8",
        serde_json::to_vec(&body).unwrap_or_default(),
    )
}
