//! The WebSocket carrier: JSON-RPC 2.0, one object per text frame.
//!
//! Contract section 4.1. This is the carrier the fire UI speaks, and it moves
//! exactly what the stdio carrier moves: [`Codec`] decides what a frame means,
//! and this module decides only how a string crosses a socket.
//!
//! Three properties are the carrier's own. Frames are handled concurrently, so
//! `agent.interrupt` is read and answered while the `agent.prompt` it
//! interrupts is still running. A push written while a call is in flight
//! reaches the peer before that call's answer does. And each connection gets
//! its own [`Codec`], because the handshake is connection state: two peers on
//! one server greet the server separately.

use std::sync::Arc;

use futures_util::stream::{SplitSink, StreamExt};
use futures_util::SinkExt;
use std::sync::Mutex;
use tetanus_protocol::methods::{Engine, EventSink};
use tetanus_protocol::rpc::ErrorCode;
use tokio::io::{AsyncRead, AsyncWrite};

use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

use crate::auth::{Auth, Refusal};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::WebSocketStream;

use crate::{Codec, Frames};

/// Accept connections on `listener` and serve each one, forever.
///
/// Returns only when the accept itself fails, which is the listener being
/// unusable rather than a peer misbehaving. One connection's failure - a
/// handshake that is not a WebSocket handshake, a socket that resets - ends
/// that connection and no other.
pub async fn serve(engine: Arc<dyn Engine>, listener: TcpListener) -> std::io::Result<()> {
    serve_as(engine, listener, Auth::default()).await
}

/// The same, under a stated posture.
///
/// Contract section 4.1.2. [`Auth::default`] is the weakest one offered and
/// still refuses every off-box peer and every browser origin, so `serve` is
/// safe to call and a deployment bound off-box has to say so with
/// [`Auth::require_token`] rather than get there by omission.
pub async fn serve_as(
    engine: Arc<dyn Engine>,
    listener: TcpListener,
    auth: Auth,
) -> std::io::Result<()> {
    let auth = Arc::new(auth);
    loop {
        let (stream, peer) = listener.accept().await?;
        let engine = Arc::clone(&engine);
        let auth = Arc::clone(&auth);
        tokio::spawn(async move {
            let _ = connection_as(engine, stream, &auth, peer.ip()).await;
        });
    }
}

/// Serve one connection: perform the WebSocket handshake, then move frames
/// until the peer closes.
///
/// The connection's subscriptions are closed on the way out, so a peer that
/// hangs up does not leave the engine pushing into a socket nobody reads.
pub async fn connection<S>(engine: Arc<dyn Engine>, stream: S) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    connection_as(
        engine,
        stream,
        &Auth::default(),
        std::net::IpAddr::from([127, 0, 0, 1]),
    )
    .await
}

/// Serve one connection whose peer has to earn it.
///
/// The handshake is refused *before* the upgrade, which is why no
/// `ErrorCode` covers this: an unauthenticated peer never reaches the
/// JSON-RPC layer, so the frame that would carry one is never sent. What it
/// gets is an HTTP status, and a deliberately uninformative one - a prober
/// that could tell "no token" from "wrong token" would learn whether it had
/// found a server expecting the token it was guessing.
pub async fn connection_as<S>(
    engine: Arc<dyn Engine>,
    stream: S,
    auth: &Auth,
    peer: std::net::IpAddr,
) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let refused: Arc<Mutex<Option<Refusal>>> = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&refused);
    let posture = auth.clone();

    let socket = tokio_tungstenite::accept_hdr_async(
        stream,
        // The `Err` type is `ErrorResponse`, which is what tungstenite's
        // handshake callback requires; it cannot be boxed without changing a
        // signature this crate does not own.
        #[allow(clippy::result_large_err)]
        move |request: &Request, response: Response| {
            let headers = request.headers();
            let mut presented = Auth::present(
                headers
                    .get("sec-websocket-protocol")
                    .and_then(|v| v.to_str().ok()),
                request.uri().query(),
            );
            presented.origin = headers
                .get("origin")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);

            match posture.admit(peer, &presented) {
                Ok(()) => Ok(response),
                Err(refusal) => {
                    *seen.lock().expect("refusal") = Some(refusal.clone());
                    let mut denial = ErrorResponse::new(None);
                    *denial.status_mut() =
                        StatusCode::from_u16(refusal.status()).unwrap_or(StatusCode::UNAUTHORIZED);
                    Err(denial)
                }
            }
        },
    )
    .await?;
    let codec = Arc::new(Codec::new(engine));
    // Unbounded for the same reason the stdio carrier is: the session log is
    // the stream (section 7.2), so a dropped event is a hole in it, and
    // blocking the turn that is pushing is no better.
    let (frames, queued) = mpsc::unbounded_channel();
    let (outgoing, incoming) = socket.split();
    let writer = tokio::spawn(write_frames(outgoing, queued));
    let sink: Arc<dyn EventSink> = Arc::new(Frames(frames.clone()));

    let read = read_frames(&codec, &sink, &frames, incoming).await;

    codec.close().await;
    // The sink may still be held by the engine, so the writer is told to stop
    // rather than left waiting for the channel to close on its own.
    let _ = frames.send(None);
    writer.await.expect("the writer task does not panic")?;
    read
}

type Incoming<S> = futures_util::stream::SplitStream<WebSocketStream<S>>;

async fn read_frames<S>(
    codec: &Arc<Codec>,
    sink: &Arc<dyn EventSink>,
    frames: &UnboundedSender<Option<String>>,
    mut incoming: Incoming<S>,
) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut inflight = JoinSet::new();

    let read = loop {
        match incoming.next().await {
            Some(Ok(Message::Text(text))) => {
                let (codec, sink, frames) = (codec.clone(), sink.clone(), frames.clone());
                inflight.spawn(async move {
                    if let Some(answer) = codec.frame(text.as_str(), sink).await {
                        let _ = frames.send(Some(answer));
                    }
                });
            }
            // Contract 1.0 puts one JSON object in one *text* frame. Serving a
            // binary frame that happens to carry the same bytes would leave
            // two clients disagreeing about the framing, and dropping it would
            // leave this one waiting, so it is refused and said why.
            Some(Ok(Message::Binary(_))) => {
                let _ = frames.send(Some(refusal(
                    "this carrier is text-framed: contract 1.0 puts one JSON object in one text \
                     frame",
                )));
            }
            // A ping is answered by the protocol layer, and a pong answers one
            // this side sent. Neither carries a frame.
            Some(Ok(_)) => continue,
            Some(Err(error)) if closed(&error) => break Ok(()),
            Some(Err(error)) => break Err(error),
            None => break Ok(()),
        }
    };

    // A call still running has a peer waiting for it, even when that peer has
    // stopped writing, so the answer is written before the connection ends.
    while inflight.join_next().await.is_some() {}
    read
}

async fn write_frames<S>(
    mut outgoing: SplitSink<WebSocketStream<S>, Message>,
    mut queued: UnboundedReceiver<Option<String>>,
) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(Some(frame)) = queued.recv().await {
        // `send` flushes: a peer waiting on an answer cannot know to wait for
        // more, and a buffered push is a push that has not happened.
        if let Err(error) = outgoing.send(Message::Text(frame.into())).await {
            return if closed(&error) { Ok(()) } else { Err(error) };
        }
    }
    // Say goodbye rather than dropping the socket, so a peer can tell a
    // finished connection from a lost one.
    match outgoing.close().await {
        Err(error) if !closed(&error) => Err(error),
        _ => Ok(()),
    }
}

/// A peer that is already gone. Not this side's failure to report: the
/// connection is on its way out either way. A peer that vanished without a
/// close handshake counts, because a dropped socket is how a process that died
/// hangs up; every other protocol error is a real one and is reported.
fn closed(error: &Error) -> bool {
    matches!(
        error,
        Error::ConnectionClosed
            | Error::AlreadyClosed
            | Error::Protocol(ProtocolError::ResetWithoutClosingHandshake)
    )
}

/// Section 4.1: a frame the server cannot correlate is still answered, with
/// `id: null`, because a client that is waiting has to be released.
fn refusal(message: &str) -> String {
    serde_json::to_string(&crate::refusal(ErrorCode::InvalidRequest, message))
        .expect("an error object always serializes")
}
