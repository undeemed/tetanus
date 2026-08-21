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

use tetanus_host::{
    Browse, Handler, Listing, Pattern, PickerError, Registered, Request, Response, Status, Taken,
    WebServer,
};
use tetanus_protocol::methods::{AgentStatusPush, Engine, EventSink, SessionEventPush};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_rpc::auth::{Auth, Presented};
use tetanus_rpc::Codec;

/// Where the bridge lives. A prefix, because the method is the rest of it.
pub const PREFIX: &str = "/api/";

/// The media type every call must declare, per the argument above.
const JSON: &str = "application/json";

/// The methods that are the host's own rather than the engine's.
///
/// Upstream splits its gateway the same way - `HostApi` beside `SessionsApi` -
/// and the split is real: choosing a directory is a question about the machine
/// this server runs on, not about a conversation. Routing them here rather
/// than through the codec keeps the engine's method table the contract's, with
/// nothing in it that the engine cannot answer.
mod host {
    pub const LIST: &str = "host.listDirectory";
    pub const CREATE: &str = "host.createDirectory";
}

/// Mount the bridge on a carrier, under the same posture as the socket.
///
/// The same `Auth`, deliberately. A door with a lock beside a door without one
/// is a room with no lock: this carrier reaches the whole `Engine` exactly as
/// the socket does - start turns, read every journal, read the resolved
/// configuration - so a deployment that stated a token and left the POSTs open
/// would have stated nothing at all.
pub fn mount(
    server: &WebServer,
    engine: Arc<dyn Engine>,
    auth: Arc<Auth>,
) -> Result<Registered, Taken> {
    let codec = Arc::new(Codec::new(engine));
    let picker = Browse::default();
    let handler: Handler = Arc::new(move |request| {
        let codec = Arc::clone(&codec);
        let auth = Arc::clone(&auth);
        Box::pin(async move { answer(codec, picker, auth, request).await })
    });
    server.register(Pattern::Prefix, PREFIX, handler)
}

/// One call, from the request that carried it to the answer that goes back.
async fn answer(codec: Arc<Codec>, picker: Browse, auth: Arc<Auth>, request: Request) -> Response {
    if request.method != "POST" {
        return refused(
            Status::MethodNotAllowed,
            ErrorCode::InvalidRequest,
            "a call is a POST",
        )
        .with("allow", "POST");
    }
    // Who is asking, before what they are asking for. A refusal here never
    // reaches the JSON-RPC layer, which is §4.1.2's own arrangement for the
    // socket and is why an unauthenticated peer gets a status rather than an
    // error frame.
    let presented = Presented {
        token: request
            .query("token")
            .map(str::to_string)
            .or_else(|| bearer(&request)),
        origin: request.header("origin").map(str::to_string),
    };
    if auth.admit(request.peer, &presented).is_err() {
        return refused(
            Status::Unauthorized,
            ErrorCode::InvalidRequest,
            "this carrier is not open to you",
        );
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

    // The host's own methods are answered here, before the codec sees a frame:
    // they are questions about this machine, and the engine's method table
    // stays the contract's.
    if let Some(answer) = picked(picker, method, &params) {
        return answer;
    }

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

/// A token from the one header a non-browser client would think to use.
///
/// A browser cannot set headers on a WebSocket handshake, which is why the
/// socket reads the URL; a `fetch` can, and a caller with a shell can too, so
/// both spellings are accepted here and neither is required.
fn bearer(request: &Request) -> Option<String> {
    request
        .header("authorization")?
        .strip_prefix("Bearer ")
        .map(str::to_string)
}

/// The two host methods, or `None` for a call that is not one of them.
fn picked(picker: Browse, method: &str, params: &serde_json::Value) -> Option<Response> {
    match method {
        host::LIST => {
            // An absent path is the account's home, which is where a chooser
            // that did not say where to open should open.
            let path = params.get("path").and_then(|path| path.as_str());
            Some(match picker.list(path.map(std::path::Path::new)) {
                Ok(listing) => result(listed(&listing)),
                Err(err) => result_error(&refusal(&err)),
            })
        }
        host::CREATE => {
            let (Some(path), Some(name)) = (
                params.get("path").and_then(|path| path.as_str()),
                params.get("name").and_then(|name| name.as_str()),
            ) else {
                return Some(result_error(&RpcError::new(
                    ErrorCode::InvalidParams,
                    "a directory is made at a path, under a name",
                )));
            };
            Some(match picker.create(std::path::Path::new(path), name) {
                Ok(entry) => result(serde_json::json!({
                    "name": entry.name,
                    "path": entry.path,
                    "hidden": entry.hidden,
                })),
                Err(err) => result_error(&refusal(&err)),
            })
        }
        _ => None,
    }
}

/// A listing, as the wire carries it.
fn listed(listing: &Listing) -> serde_json::Value {
    let rows = |entries: &[tetanus_host::Entry]| {
        entries
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.name,
                    "path": entry.path,
                    "hidden": entry.hidden,
                })
            })
            .collect::<Vec<_>>()
    };
    serde_json::json!({
        "path": listing.path,
        "entries": rows(&listing.entries),
        "crumbs": rows(&listing.crumbs),
        "truncated": listing.truncated,
    })
}

/// The picker's three failures, mapped one to one onto codes a caller can act
/// on - which is what upstream's gateway does with the same three.
///
/// The subject path travels in `data`, because a chooser showing "cannot be
/// read" with nothing named is a dialog the reader cannot argue with.
fn refusal(err: &PickerError) -> RpcError {
    let (code, path) = match err {
        // A path that cannot be read is the filesystem's answer, which §4.5
        // spells `Io` and gives a `path` field for exactly this.
        PickerError::Unreadable(path) => (ErrorCode::Io, path),
        // Already there is not a fault of the machine: the caller named a
        // directory that exists, and the argument is what was wrong.
        PickerError::Exists(path) => (ErrorCode::InvalidParams, path),
        PickerError::CreateFailed(path) => (ErrorCode::Io, path),
    };
    RpcError::new(code, err.to_string())
        .with_data(serde_json::json!({ "path": path.display().to_string() }))
}

/// A host method's answer, in the envelope every other answer uses.
fn result(value: serde_json::Value) -> Response {
    envelope(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": value }))
}

/// The same, for one that failed. Still 200: the carrier worked.
fn result_error(error: &RpcError) -> Response {
    envelope(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": serde_json::to_value(error).unwrap_or(serde_json::Value::Null),
    }))
}

/// The bytes of an answer.
fn envelope(body: serde_json::Value) -> Response {
    Response::body(
        Status::Ok,
        "application/json; charset=utf-8",
        serde_json::to_vec(&body).unwrap_or_default(),
    )
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
