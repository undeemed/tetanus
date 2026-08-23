//! The other end of the wire: an ACP client that drives an agent over stdio.
//!
//! The bridge in [`crate::bridge`] is the agent half. Until something speaks to
//! it as a peer rather than as a test double, the protocol is a shape nobody
//! has exercised: a codec that answers frames a suite wrote is not the same
//! claim as a process that spawns another process, negotiates, prompts, reads
//! what comes back and shuts it down. Upstream ships this half too, as the
//! subagent driver its own bridge names as its primary client.
//!
//! Four things make this a client rather than a frame writer, and each is a
//! failure mode that only appears once a real process is on the other end.
//!
//! **It answers.** ACP's `session/request_permission` is a request *from* the
//! agent, and an agent that asked one and got nothing back waits for ever. A
//! client that only sends is not a client, so this one carries a
//! [`PermissionPolicy`] and answers every question with it.
//!
//! **It demultiplexes.** Responses, notifications and inbound requests arrive
//! on one stream, interleaved, and a prompt's `session/update` frames arrive
//! *before* the answer to the prompt. One reader task owns the pipe and routes
//! by shape, because two readers on one pipe is a race over half a line.
//!
//! **Every wait is bounded.** A child that stops answering is the
//! characteristic failure here, and it is indistinguishable from a slow model
//! unless something is counting. Every request carries a deadline, and so does
//! the shutdown.
//!
//! **It reaps.** A dropped handle that leaves a child running is a process
//! nobody will ever kill, so [`AcpClient::close`] walks stdin-EOF to kill and
//! is idempotent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tetanus_protocol::rpc::{Id, Payload, Request, Response, RpcError, V2};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};

use crate::wire::{
    agent, method, ContentBlock, InitializeRequest, InitializeResponse, LoadSessionRequest,
    NewSessionRequest, NewSessionResponse, PermissionOutcome, PromptRequest, PromptResponse,
    RequestPermissionRequest, SessionNotification, SessionUpdate, StopReason, ALLOW_ONCE,
    PROTOCOL_VERSION, REJECT_ONCE,
};

/// How long any one call waits before it is a hang rather than slow work.
///
/// A prompt runs a model, so this is generous. It is not absent, because the
/// failure it bounds - a child that has stopped answering - looks exactly like
/// a model thinking, and only a clock can tell them apart.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// What this client answers a permission question with.
///
/// Fail-closed by default. A client that is automating has to decide in
/// advance, and the safe default for "I did not think about this" is refusal:
/// an agent that is denied reports a denied tool call, where an agent that is
/// wrongly allowed has already run the command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PermissionPolicy {
    /// Answer every question `allow-once`. One call at a time - this is not a
    /// standing grant, because ACP has no way to express one that this bridge
    /// would honour.
    AllowOnce,
    /// Answer every question `reject-once`.
    #[default]
    Reject,
}

impl PermissionPolicy {
    fn option_id(self) -> &'static str {
        match self {
            Self::AllowOnce => ALLOW_ONCE,
            Self::Reject => REJECT_ONCE,
        }
    }
}

/// What to spawn, and how.
#[derive(Debug, Clone)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Working directory for the child. `None` inherits this process's.
    pub cwd: Option<PathBuf>,
    /// Environment entries to set on the child, on top of the inherited ones.
    pub env: Vec<(String, String)>,
}

impl Launch {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// Why a call did not produce an answer.
#[derive(Debug)]
pub enum ClientError {
    /// The agent answered, and the answer was a refusal. Carried whole, code
    /// and `data` intact, because the agent's vocabulary is the caller's
    /// documentation for what went wrong.
    Refused(RpcError),
    /// A bound elapsed. Names the call, because "something timed out" is the
    /// least useful sentence a client can produce.
    TimedOut { call: String, after: Duration },
    /// The child is gone, or was never there.
    Transport(String),
    /// The agent answered something the protocol does not describe.
    Protocol(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(error) => write!(f, "{} (code {})", error.message, error.code),
            Self::TimedOut { call, after } => {
                write!(f, "`{call}` did not answer within {after:?}")
            }
            Self::Transport(why) => write!(f, "the agent is gone: {why}"),
            Self::Protocol(why) => write!(f, "the agent broke protocol: {why}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// One prompt's outcome: why it stopped, and everything the agent said while
/// it ran.
#[derive(Debug, Clone)]
pub struct PromptOutcome {
    pub stop_reason: StopReason,
    /// The `session/update` payloads for this session, in arrival order.
    pub updates: Vec<SessionUpdate>,
}

impl PromptOutcome {
    /// The agent's committed text, in order. What a caller wanting "the
    /// answer" reads.
    pub fn messages(&self) -> Vec<String> {
        self.updates
            .iter()
            .filter_map(|update| match update {
                SessionUpdate::AgentMessageChunk {
                    content: ContentBlock::Text { text },
                } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every tool the agent called, in order.
    pub fn tool_calls(&self) -> Vec<(String, String)> {
        self.updates
            .iter()
            .filter_map(|update| match update {
                SessionUpdate::ToolCall {
                    tool_call_id,
                    title,
                    ..
                } => Some((tool_call_id.clone(), title.clone())),
                _ => None,
            })
            .collect()
    }
}

/// Shared state the reader task and the caller both touch.
struct Wire {
    /// `None` once the client has closed it.
    ///
    /// An `Option` rather than a plain handle because closing the pipe is the
    /// only way to tell the agent to stop, and a pipe is closed by *dropping*
    /// the writer. Calling `shutdown` while this struct still owns the handle
    /// leaves the file descriptor open, the child never reaches end of file,
    /// and every teardown waits out the kill fallback instead - which works,
    /// looks fine, and turns a graceful stop into a killed process.
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    pending: Mutex<BTreeMap<String, oneshot::Sender<Result<serde_json::Value, RpcError>>>>,
    requests: AtomicU64,
    policy: PermissionPolicy,
    /// Permission questions this client answered, for a caller that wants to
    /// assert it was asked.
    answered: Mutex<Vec<String>>,
}

impl Wire {
    async fn write(&self, frame: &str) -> Result<(), ClientError> {
        let mut held = self.stdin.lock().await;
        let stdin = held
            .as_mut()
            .ok_or_else(|| ClientError::Transport("this client is closed".into()))?;
        stdin
            .write_all(frame.as_bytes())
            .await
            .map_err(|err| ClientError::Transport(err.to_string()))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|err| ClientError::Transport(err.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|err| ClientError::Transport(err.to_string()))
    }
}

/// An ACP agent, running as a child process, driven as a peer.
pub struct AcpClient {
    child: Option<Child>,
    wire: Arc<Wire>,
    updates: mpsc::UnboundedReceiver<SessionNotification>,
    reader: tokio::task::JoinHandle<()>,
    timeout: Duration,
}

impl AcpClient {
    /// Spawn an agent and start reading from it.
    pub async fn spawn(launch: Launch, policy: PermissionPolicy) -> Result<Self, ClientError> {
        let mut command = Command::new(&launch.program);
        command
            .args(&launch.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Left alone on purpose: an agent's diagnostics are its operator's,
            // and swallowing them is how a misconfigured child becomes a silent
            // timeout with nothing to read.
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(cwd) = &launch.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &launch.env {
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(|err| {
            ClientError::Transport(format!("{}: {err}", launch.program.display()))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ClientError::Transport("the child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClientError::Transport("the child has no stdout".into()))?;

        let wire = Arc::new(Wire {
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            pending: Mutex::new(BTreeMap::new()),
            requests: AtomicU64::new(0),
            policy,
            answered: Mutex::new(Vec::new()),
        });
        let (updates_out, updates) = mpsc::unbounded_channel();
        let reader = tokio::spawn(read_frames(Arc::clone(&wire), stdout, updates_out));

        Ok(Self {
            child: Some(child),
            wire,
            updates,
            reader,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Set the per-call deadline. Never unset: see [`DEFAULT_TIMEOUT`].
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The permission questions this client has answered, in order.
    pub fn answered(&self) -> Vec<String> {
        self.wire.answered.lock().expect("answered").clone()
    }

    /// Negotiate. The first call on a connection, as ACP requires.
    pub async fn initialize(&mut self) -> Result<InitializeResponse, ClientError> {
        let answered = self
            .call(
                method::INITIALIZE,
                InitializeRequest {
                    protocol_version: PROTOCOL_VERSION,
                    client_capabilities: None,
                },
            )
            .await?;
        decode(answered)
    }

    /// Open a session. `cwd` must be absolute; ACP says so and the agent
    /// checks, so this checks too rather than paying a round trip to be told.
    pub async fn new_session(&mut self, cwd: &Path) -> Result<String, ClientError> {
        Self::admit_cwd(cwd)?;
        let answered = self
            .call(
                method::SESSION_NEW,
                NewSessionRequest {
                    cwd: cwd.to_string_lossy().into_owned(),
                    mcp_servers: Vec::new(),
                },
            )
            .await?;
        let created: NewSessionResponse = decode(answered)?;
        Ok(created.session_id)
    }

    /// Re-open a session the agent already has, and collect its history.
    ///
    /// The history arrives as `session/update` notifications *before* the
    /// answer, exactly as a turn's updates do, so the drain-then-collect shape
    /// is the same one [`Self::prompt`] uses and for the same reason: once the
    /// answer is in hand the notifications are already queued.
    pub async fn load_session(
        &mut self,
        session_id: &str,
        cwd: &Path,
    ) -> Result<Vec<SessionUpdate>, ClientError> {
        Self::admit_cwd(cwd)?;
        while self.updates.try_recv().is_ok() {}

        self.call(
            method::SESSION_LOAD,
            LoadSessionRequest {
                session_id: session_id.to_string(),
                cwd: cwd.to_string_lossy().into_owned(),
                mcp_servers: Vec::new(),
            },
        )
        .await?;

        let mut history = Vec::new();
        while let Ok(notification) = self.updates.try_recv() {
            if notification.session_id == session_id {
                history.push(notification.update);
            }
        }
        Ok(history)
    }

    /// ACP requires an absolute `cwd`, and the agent checks it too; checking
    /// here as well means a caller learns of its own mistake without paying a
    /// round trip, and the agent's check remains the one that binds.
    fn admit_cwd(cwd: &Path) -> Result<(), ClientError> {
        if cwd.is_absolute() {
            return Ok(());
        }
        Err(ClientError::Protocol(format!(
            "`cwd` is an absolute path, and `{}` is not",
            cwd.display()
        )))
    }

    /// Send a prompt and collect the turn.
    ///
    /// The updates buffer is drained first, so what comes back is this turn's
    /// and not the tail of the last one. The agent writes every
    /// `session/update` before the answer to the prompt - the carrier promises
    /// that ordering - so once the answer has arrived the updates are already
    /// queued and draining is enough.
    pub async fn prompt(
        &mut self,
        session_id: &str,
        blocks: Vec<ContentBlock>,
    ) -> Result<PromptOutcome, ClientError> {
        while self.updates.try_recv().is_ok() {}

        let answered = self
            .call(
                method::SESSION_PROMPT,
                PromptRequest {
                    session_id: session_id.to_string(),
                    prompt: blocks,
                },
            )
            .await?;
        let settled: PromptResponse = decode(answered)?;

        let mut updates = Vec::new();
        while let Ok(notification) = self.updates.try_recv() {
            if notification.session_id == session_id {
                updates.push(notification.update);
            }
        }
        Ok(PromptOutcome {
            stop_reason: settled.stop_reason,
            updates,
        })
    }

    /// Ask the agent to stop. One-way: ACP's cancel is a notification, so
    /// there is nothing to wait for and nothing to fail.
    pub async fn cancel(&self, session_id: &str) -> Result<(), ClientError> {
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method::SESSION_CANCEL,
            "params": { "sessionId": session_id },
        });
        self.wire.write(&frame.to_string()).await
    }

    /// Close stdin, wait for the child, and kill it if it will not go.
    ///
    /// Idempotent. The ladder matters: closing stdin is how a well-behaved
    /// agent is told to stop, and killing without asking first would deny it
    /// the chance to flush a journal. Waiting for ever would hang the caller,
    /// so the wait is bounded and the kill is what the bound leads to.
    pub async fn close(&mut self) -> Result<(), ClientError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        // Dropping the writer closes the pipe, which is end-of-file to the
        // carrier on the other side. Taken out of the `Option` and dropped
        // rather than merely shut down: while this struct holds the handle the
        // descriptor stays open and the child waits for input that will never
        // come.
        {
            let mut held = self.wire.stdin.lock().await;
            if let Some(mut stdin) = held.take() {
                let _ = stdin.shutdown().await;
                drop(stdin);
            }
        }
        let ended =
            tokio::time::timeout(self.timeout.min(Duration::from_secs(10)), child.wait()).await;
        match ended {
            Ok(Ok(_)) => {}
            // Either it will not end or waiting failed; either way this process
            // is not leaving a child behind.
            _ => {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
        self.reader.abort();
        // Every waiter is released rather than left for its own deadline: the
        // agent is gone, and that answer is available now.
        let waiting = std::mem::take(&mut *self.wire.pending.lock().expect("pending"));
        drop(waiting);
        Ok(())
    }

    async fn call<T: serde::Serialize>(
        &self,
        method: &str,
        params: T,
    ) -> Result<serde_json::Value, ClientError> {
        let id = format!(
            "tetanus-acp-client-{}",
            self.wire.requests.fetch_add(1, Ordering::Relaxed)
        );
        let (sender, receiver) = oneshot::channel();
        self.wire
            .pending
            .lock()
            .expect("pending")
            .insert(id.clone(), sender);

        let request = Request {
            jsonrpc: V2,
            id: Id::Text(id.clone()),
            method: method.to_string(),
            params: Some(
                serde_json::to_value(params)
                    .map_err(|err| ClientError::Protocol(err.to_string()))?,
            ),
        };
        let frame = serde_json::to_string(&request)
            .map_err(|err| ClientError::Protocol(err.to_string()))?;
        self.wire.write(&frame).await?;

        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(refused))) => Err(ClientError::Refused(refused)),
            // The sender was dropped: the reader task ended, which means the
            // pipe closed.
            Ok(Err(_)) => Err(ClientError::Transport(
                "the agent closed the connection".into(),
            )),
            Err(_) => {
                self.wire.pending.lock().expect("pending").remove(&id);
                Err(ClientError::TimedOut {
                    call: method.to_string(),
                    after: self.timeout,
                })
            }
        }
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        // `kill_on_drop` handles the child; this stops the reader from holding
        // a pipe open behind it.
        self.reader.abort();
    }
}

/// The one reader on the pipe: route every frame by its shape.
async fn read_frames(
    wire: Arc<Wire>,
    stdout: tokio::process::ChildStdout,
    updates: mpsc::UnboundedSender<SessionNotification>,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&line) else {
            // A line that is not JSON is not this client's to interpret. The
            // agent's stderr is inherited, so whatever produced it is already
            // visible to the operator.
            continue;
        };
        let has_method = frame.get("method").is_some();
        let has_id = frame.get("id").is_some();

        match (has_method, has_id) {
            // A request from the agent: answer it.
            (true, true) => answer(&wire, &frame).await,
            // A notification.
            (true, false) => {
                if frame["method"] == serde_json::json!(agent::SESSION_UPDATE) {
                    if let Ok(notification) =
                        serde_json::from_value::<SessionNotification>(frame["params"].clone())
                    {
                        let _ = updates.send(notification);
                    }
                }
            }
            // A response to something this client asked.
            (false, true) => settle(&wire, frame),
            (false, false) => {}
        }
    }
    // End of file: the agent is gone. Dropping every waiter releases each call
    // now rather than at its own deadline.
    wire.pending.lock().expect("pending").clear();
}

fn settle(wire: &Arc<Wire>, frame: serde_json::Value) {
    let Ok(response) = serde_json::from_value::<Response>(frame) else {
        return;
    };
    let Id::Text(id) = &response.id else {
        return;
    };
    let Some(waiter) = wire.pending.lock().expect("pending").remove(id) else {
        return;
    };
    let _ = waiter.send(match response.payload {
        Payload::Result(value) => Ok(value),
        Payload::Error(error) => Err(error),
    });
}

/// Answer a request the agent made of this client.
///
/// Only `session/request_permission` is understood. Anything else is refused
/// with `MethodNotFound` rather than ignored, because an agent waiting on an
/// answer that never comes is the hang this whole file exists to avoid - and a
/// refusal is an answer.
async fn answer(wire: &Arc<Wire>, frame: &serde_json::Value) {
    let id = frame["id"].clone();
    let method = frame["method"].as_str().unwrap_or_default();

    let payload = if method == agent::REQUEST_PERMISSION {
        let asked: Result<RequestPermissionRequest, _> =
            serde_json::from_value(frame["params"].clone());
        match asked {
            Ok(asked) => {
                wire.answered
                    .lock()
                    .expect("answered")
                    .push(asked.tool_call.tool_call_id.clone());
                let chosen = wire.policy.option_id();
                // Only an option the agent actually offered is chosen. A client
                // that answered with an id the agent never listed would be
                // making up protocol, and the agent is right to deny it.
                let offered = asked
                    .options
                    .iter()
                    .any(|option| option.option_id == chosen);
                let outcome = if offered {
                    PermissionOutcome::Selected {
                        option_id: chosen.to_string(),
                    }
                } else {
                    PermissionOutcome::Cancelled
                };
                serde_json::json!({ "outcome": outcome })
            }
            Err(err) => serde_json::json!({
                "error": { "code": -32602, "message": err.to_string() }
            }),
        }
    } else {
        serde_json::json!({
            "error": { "code": -32601, "message": format!("no method `{method}`") }
        })
    };

    let frame = if payload.get("error").is_some() {
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": payload["error"] })
    } else {
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": payload })
    };
    let _ = wire.write(&frame.to_string()).await;
}

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, ClientError> {
    serde_json::from_value(value).map_err(|err| ClientError::Protocol(err.to_string()))
}
