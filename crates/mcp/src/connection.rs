//! One live conversation with one MCP server.
//!
//! The shape is an actor: a driver task owns the link, everything else holds a
//! [`Connection`] and sends it commands. That is what makes concurrent tool
//! calls possible at all - the tool pipeline dispatches parallel-safe calls at
//! once, and each is a request that must be matched to its own answer by id.
//!
//! **The driver is the only place a request id is answered.** Pending replies
//! live in the driver's own map, so no lock is held across a wait, and a
//! server that answers out of order is served correctly for free.
//!
//! **Death is broadcast once, and it reaches everyone.** When the link ends,
//! every pending call is failed with the same fault, the liveness watch flips,
//! and commands that arrive afterwards are answered with that fault rather
//! than with "the channel is closed" - a caller should read why the server is
//! gone, not that this crate's plumbing noticed.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::fault::McpFault;
use crate::link::{Departure, Link};
use crate::wire::{self, Frame};

/// A notification the server sent. The one this crate acts on is
/// [`wire::method::TOOL_LIST_CHANGED`]; the rest are carried so a host can.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub method: String,
    pub params: Value,
}

enum Command {
    Call {
        id: u64,
        method: String,
        params: Value,
        reply: oneshot::Sender<Result<Value, McpFault>>,
    },
    Notify {
        method: String,
        params: Value,
        reply: oneshot::Sender<Result<(), McpFault>>,
    },
    /// A call whose caller stopped waiting. The pending answer is dropped and
    /// the server is told, per MCP's cancellation notification.
    Cancel {
        id: u64,
        reason: String,
    },
    Close {
        reply: oneshot::Sender<Departure>,
    },
}

/// A handle to one server. Cloneable: the bridge hands one to every tool.
#[derive(Clone, Debug)]
pub struct Connection {
    server: Arc<str>,
    commands: mpsc::Sender<Command>,
    live: watch::Receiver<bool>,
    notices: broadcast::Sender<Notice>,
    next_id: Arc<AtomicU64>,
}

/// How many notices are held for a subscriber that is slow to read. A host
/// that misses one re-lists rather than waiting: the notification is a hint
/// that something changed, never the change itself.
const NOTICE_BACKLOG: usize = 32;

impl Connection {
    /// Start driving `link`. The task lives until the link ends, every handle
    /// is dropped, or [`Connection::close`] is called.
    pub fn open(server: impl Into<String>, link: Link) -> Self {
        let server: Arc<str> = Arc::from(server.into());
        let (commands, inbox) = mpsc::channel(64);
        let (live_tx, live) = watch::channel(true);
        let (notices, _) = broadcast::channel(NOTICE_BACKLOG);
        tokio::spawn(drive(
            Arc::clone(&server),
            link,
            inbox,
            live_tx,
            notices.clone(),
        ));
        Self {
            server,
            commands,
            live,
            notices,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    /// Whether the link is still there. False the moment the driver knows
    /// otherwise, which is before the next call fails.
    pub fn is_live(&self) -> bool {
        *self.live.borrow()
    }

    /// Wait until the link ends. Returns immediately if it already has.
    ///
    /// This is what the supervisor waits on: a reconnect that polled would
    /// either be slow or be a spin.
    pub async fn departed(&self) {
        let mut live = self.live.clone();
        while *live.borrow_and_update() {
            if live.changed().await.is_err() {
                return;
            }
        }
    }

    /// Notices from the server, from now on.
    pub fn notices(&self) -> broadcast::Receiver<Notice> {
        self.notices.subscribe()
    }

    /// Make one request and wait `within` for its answer.
    ///
    /// A budget that runs out fails this call and nothing else: the server is
    /// told to forget the request, the connection stays up, and a late answer
    /// is dropped. One slow tool must not cost every other tool its server.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        within: Duration,
    ) -> Result<Value, McpFault> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Call {
                id,
                method: method.to_string(),
                params,
                reply,
            })
            .await
            .map_err(|_| self.gone())?;
        match tokio::time::timeout(within, answer).await {
            Ok(Ok(outcome)) => outcome,
            // The driver dropped the reply without sending: it is on its way
            // out and the fault it knows about could not reach this caller.
            Ok(Err(_)) => Err(self.gone()),
            Err(_) => {
                let _ = self
                    .commands
                    .send(Command::Cancel {
                        id,
                        reason: format!(
                            "the client stopped waiting after {}ms",
                            within.as_millis()
                        ),
                    })
                    .await;
                Err(McpFault::Timeout {
                    method: method.to_string(),
                    after: within,
                })
            }
        }
    }

    /// Send one notification. No answer is owed, but the write can still fail.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpFault> {
        let (reply, sent) = oneshot::channel();
        self.commands
            .send(Command::Notify {
                method: method.to_string(),
                params,
                reply,
            })
            .await
            .map_err(|_| self.gone())?;
        sent.await.map_err(|_| self.gone())?
    }

    /// Stop the server and report how it went. Idempotent: a second call on a
    /// connection whose driver has finished answers [`Departure::closed`].
    pub async fn close(&self) -> Departure {
        let (reply, departed) = oneshot::channel();
        if self.commands.send(Command::Close { reply }).await.is_err() {
            return Departure::closed();
        }
        departed.await.unwrap_or_else(|_| Departure::closed())
    }

    fn gone(&self) -> McpFault {
        McpFault::Unavailable(format!("{}: the connection is closed", self.server))
    }
}

/// The driver task: one link, one pending map, one exit.
async fn drive(
    server: Arc<str>,
    link: Link,
    mut commands: mpsc::Receiver<Command>,
    live: watch::Sender<bool>,
    notices: broadcast::Sender<Notice>,
) {
    let Link {
        mut writer,
        mut reader,
        pid: _,
    } = link;

    // The reader runs in its own task so the driver can write while the peer
    // is quiet; see the note on `crate::link`.
    let (lines_tx, mut lines) = mpsc::channel::<Result<String, String>>(64);
    let pump = tokio::spawn(async move {
        while let Some(line) = reader.recv().await {
            let broken = line.is_err();
            let line = line.map_err(|source| source.to_string());
            if lines_tx.send(line).await.is_err() || broken {
                break;
            }
        }
    });

    // The method is kept beside the waiting caller so a refusal can name what
    // was refused: a server's own message rarely says which call it answers.
    let mut pending: BTreeMap<u64, (String, oneshot::Sender<Result<Value, McpFault>>)> =
        BTreeMap::new();
    let mut closer: Option<oneshot::Sender<Departure>> = None;

    let ended = loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => break McpFault::Unavailable(format!("{server}: every handle was dropped")),
                Some(Command::Close { reply }) => {
                    closer = Some(reply);
                    break McpFault::Unavailable(format!("{server}: the connection was closed"));
                }
                Some(Command::Call { id, method, params, reply }) => {
                    if let Err(source) = writer.send(&wire::request(id, &method, params)).await {
                        let fault = McpFault::Transport(format!("{server}: {source}"));
                        let _ = reply.send(Err(fault.clone()));
                        break fault;
                    }
                    pending.insert(id, (method, reply));
                }
                Some(Command::Notify { method, params, reply }) => {
                    match writer.send(&wire::notification(&method, params)).await {
                        Ok(()) => { let _ = reply.send(Ok(())); }
                        Err(source) => {
                            let fault = McpFault::Transport(format!("{server}: {source}"));
                            let _ = reply.send(Err(fault.clone()));
                            break fault;
                        }
                    }
                }
                Some(Command::Cancel { id, reason }) => {
                    pending.remove(&id);
                    let params = json!({ "requestId": id, "reason": reason });
                    if let Err(source) = writer.send(&wire::notification(wire::method::CANCELLED, params)).await {
                        break McpFault::Transport(format!("{server}: {source}"));
                    }
                }
            },
            line = lines.recv() => match line {
                None => break McpFault::Transport(format!("{server}: the server closed its output")),
                Some(Err(source)) => break McpFault::Transport(format!("{server}: {source}")),
                Some(Ok(line)) => {
                    match wire::parse(&line) {
                        Ok(Frame::Answer { id, outcome }) => match pending.remove(&id) {
                            Some((method, reply)) => {
                                let _ = reply.send(outcome.map_err(|refusal| McpFault::Server {
                                    method,
                                    code: refusal.code,
                                    message: refusal.message,
                                }));
                            }
                            // A late answer to a cancelled call, or an id this
                            // client never sent. Neither is worth a fault: the
                            // stream is still framed correctly.
                            None => tracing::debug!(server = %server, id, "an MCP answer arrived for no pending request"),
                        },
                        Ok(Frame::Notification { method, params }) => {
                            let _ = notices.send(Notice { method, params });
                        }
                        Ok(Frame::Ask { id, method }) => {
                            if let Err(source) = writer.send(&wire::unsupported(&id, &method)).await {
                                break McpFault::Transport(format!("{server}: {source}"));
                            }
                        }
                        Err(why) => break McpFault::Protocol(format!("{server}: {why}")),
                    }
                }
            },
        }
    };

    // One exit, for everyone. The order matters: nothing new may be accepted
    // as live while the peer is being stopped.
    let _ = live.send(false);
    for (_, (_, reply)) in std::mem::take(&mut pending) {
        let _ = reply.send(Err(ended.clone()));
    }
    let departure = writer.stop().await;
    pump.abort();
    if let Some(closer) = closer {
        let _ = closer.send(departure);
    }

    // Whoever calls after this reads the fault rather than a closed channel.
    while let Some(command) = commands.recv().await {
        match command {
            Command::Call { reply, .. } => {
                let _ = reply.send(Err(ended.clone()));
            }
            Command::Notify { reply, .. } => {
                let _ = reply.send(Err(ended.clone()));
            }
            Command::Close { reply } => {
                let _ = reply.send(departure);
            }
            Command::Cancel { .. } => {}
        }
    }
}
