//! The stdio carrier: JSON-RPC 2.0, one object per line, UTF-8.
//!
//! Contract section 4.1. An editor or a script drives the binary by writing a
//! frame per line and reading a frame per line. This module moves strings; the
//! meaning of them is [`Codec`]'s.
//!
//! Two properties are the carrier's own, and neither is visible to the codec:
//! frames are handled concurrently, so `agent.interrupt` is read and answered
//! while the `agent.prompt` it interrupts is still running; and a push written
//! while a call is in flight reaches the peer before that call's answer does.

use std::io;
use std::sync::Arc;

use tetanus_protocol::methods::{push, AgentStatusPush, Engine, EventSink, SessionEventPush};
use tetanus_protocol::rpc::{Notification, V2};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinSet;

use crate::Codec;

/// Serve one connection until its peer stops writing.
///
/// Returns when `input` reaches end of file, every frame in flight has been
/// answered, and every answer has been written. The connection's subscriptions
/// are closed on the way out, so a peer that hangs up does not leave the engine
/// pushing into a socket nobody reads.
pub async fn serve<R, W>(engine: Arc<dyn Engine>, input: R, output: W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let codec = Arc::new(Codec::new(engine));
    // Unbounded because the alternative is worse: a full queue would either
    // block the turn that is pushing or drop an event, and the session log is
    // the stream (section 7.2), so a dropped event is a hole in it.
    let (frames, queued) = mpsc::unbounded_channel();
    let writer = tokio::spawn(write_frames(output, queued));
    let sink: Arc<dyn EventSink> = Arc::new(Frames(frames.clone()));

    eprintln!("DBG serve start");
    let read = read_frames(&codec, &sink, &frames, input).await;
    eprintln!("DBG read done: {read:?}");

    codec.close().await;
    // The sink may still be held by the engine, so the writer is told to stop
    // rather than left waiting for the channel to close on its own.
    let _ = frames.send(None);
    writer.await.expect("the writer task does not panic")?;
    read
}

async fn read_frames<R: AsyncRead + Unpin>(
    codec: &Arc<Codec>,
    sink: &Arc<dyn EventSink>,
    frames: &UnboundedSender<Option<String>>,
    input: R,
) -> io::Result<()> {
    let mut lines = BufReader::new(input).lines();
    let mut inflight = JoinSet::new();

    let read = loop {
        match lines.next_line().await {
            // A blank line carries no frame, and answering one would be
            // answering a question nobody asked.
            Ok(Some(line)) if line.trim().is_empty() => continue,
            Ok(Some(line)) => {
                eprintln!("DBG line in: {line}");
                let (codec, sink, frames) = (codec.clone(), sink.clone(), frames.clone());
                inflight.spawn(async move {
                    let answered = codec.frame(&line, sink).await;
                    eprintln!("DBG answered: {answered:?}");
                    if let Some(answer) = answered {
                        let _ = frames.send(Some(answer));
                    }
                });
            }
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        }
    };

    // A call still running has a peer waiting for it, even when that peer has
    // stopped writing, so the answer is written before the connection ends.
    while inflight.join_next().await.is_some() {}
    read
}

async fn write_frames<W: AsyncWrite + Unpin>(
    mut output: W,
    mut queued: mpsc::UnboundedReceiver<Option<String>>,
) -> io::Result<()> {
    eprintln!("DBG writer start");
    while let Some(Some(frame)) = queued.recv().await {
        eprintln!("DBG writing {frame}");
        output.write_all(frame.as_bytes()).await?;
        output.write_all(b"\n").await?;
        // Flushed per frame: a peer waiting on an answer cannot know to wait
        // for more, and a buffered push is a push that has not happened.
        output.flush().await?;
    }
    Ok(())
}

/// The connection's [`EventSink`]: serialize the push, write it as a frame.
struct Frames(UnboundedSender<Option<String>>);

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
