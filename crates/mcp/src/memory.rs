//! A link with no process behind it: two channels and a peer handle.
//!
//! This is how the protocol is tested without a subprocess, and how an
//! in-process server would be mounted. The suite needs both this and the real
//! thing: a channel pair proves what the client does with a message, and a
//! child process proves what it does with a program - a pipe that fills, a
//! process that will not exit, an exit status.

use std::io;

use tokio::sync::mpsc;

use crate::link::{Departure, Exit, Link, LinkReader, LinkWriter};

/// The other end of a [`Link`]: what a fake server reads and writes.
pub struct Peer {
    /// Lines the client sent.
    pub from_client: mpsc::UnboundedReceiver<String>,
    /// Lines to hand the client.
    to_client: mpsc::UnboundedSender<String>,
}

impl Peer {
    /// The next line the client sent, or `None` once the client is gone.
    pub async fn recv(&mut self) -> Option<String> {
        self.from_client.recv().await
    }

    /// Send one line to the client. Fails only when the client has gone away,
    /// which a fake server may ignore.
    pub fn send(&self, line: impl Into<String>) -> bool {
        self.to_client.send(line.into()).is_ok()
    }

    /// Go away: the client's reader reaches end of stream, as if a process had
    /// exited.
    pub fn hang_up(self) {
        drop(self);
    }
}

/// A link and the peer on the other side of it.
pub fn pair() -> (Link, Peer) {
    let (client_tx, client_rx) = mpsc::unbounded_channel();
    let (peer_tx, peer_rx) = mpsc::unbounded_channel();
    let link = Link::new(
        Box::new(MemoryWriter {
            lines: Some(client_tx),
        }),
        Box::new(MemoryReader { lines: peer_rx }),
    );
    let peer = Peer {
        from_client: client_rx,
        to_client: peer_tx,
    };
    (link, peer)
}

struct MemoryWriter {
    lines: Option<mpsc::UnboundedSender<String>>,
}

#[async_trait::async_trait]
impl LinkWriter for MemoryWriter {
    async fn send(&mut self, line: &str) -> io::Result<()> {
        let lines = self
            .lines
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "the peer is gone"))?;
        lines
            .send(line.to_string())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the peer is gone"))
    }

    async fn stop(&mut self) -> Departure {
        drop(self.lines.take());
        Departure {
            exit: Exit::Closed,
            pid: None,
        }
    }
}

struct MemoryReader {
    lines: mpsc::UnboundedReceiver<String>,
}

#[async_trait::async_trait]
impl LinkReader for MemoryReader {
    async fn recv(&mut self) -> Option<io::Result<String>> {
        self.lines.recv().await.map(Ok)
    }
}
