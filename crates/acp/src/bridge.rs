//! The bridge: one ACP connection, driving one engine.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tetanus_protocol::methods::{
    AgentPromptParams, AgentStatusPush, Engine, EventSink, SessionCreateParams, SessionEventPush,
    SessionRef, SessionSubscribeParams, SessionUnsubscribeParams,
};
use tetanus_protocol::rpc::{
    ErrorCode, Id, Message, Notification, Payload, Request, Response, RpcError, V2,
};
use tetanus_protocol::types::ApprovalOutcome;
use tetanus_rpc::{FrameHandler, FrameSink};
use tokio::sync::oneshot;

use crate::content::admit;
use crate::updates::updates_of;
use crate::wire::{
    agent, method, AgentCapabilities, AgentInfo, CancelNotification, InitializeRequest,
    InitializeResponse, NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOutcome,
    PermissionToolCall, PromptCapabilities, PromptRequest, PromptResponse,
    RequestPermissionRequest, RequestPermissionResponse, SessionNotification, StopReason,
    AGENT_NAME, ALLOW_ONCE, PROTOCOL_VERSION, REJECT_ONCE,
};

/// One ACP session this bridge owns.
struct Record {
    /// True while a `session/prompt` is being served. ACP allows one at a time
    /// per session, and the second would race the first for the session's turn
    /// slot and lose with an error the client cannot interpret.
    inflight: bool,
    /// Set by `session/cancel`. Read at settlement, because cancellation is
    /// asynchronous with the turn and the turn may finish first.
    cancelled: bool,
}

#[derive(Default)]
struct State {
    initialized: bool,
    closed: bool,
    /// Keyed by session id, which is the engine's own. A prompt naming an id
    /// this connection did not create is refused rather than served: loading
    /// and resuming are not part of this bridge, so an id it does not know is
    /// one it cannot vouch for.
    sessions: BTreeMap<String, Record>,
}

/// An ACP agent over a tetanus engine.
///
/// One per connection, for the reason `crates/rpc`'s codec is one per
/// connection: `initialize` is connection state, and so is the set of sessions
/// this peer opened and may therefore prompt.
pub struct AcpBridge {
    engine: Arc<dyn Engine>,
    state: Mutex<State>,
    /// Correlation for requests *this* side makes of the client.
    pending: Mutex<BTreeMap<String, oneshot::Sender<Result<serde_json::Value, RpcError>>>>,
    requests: AtomicU64,
}

impl AcpBridge {
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        Self {
            engine,
            state: Mutex::new(State::default()),
            pending: Mutex::new(BTreeMap::new()),
            requests: AtomicU64::new(0),
        }
    }

    /// Answer one frame, exactly as `crates/rpc`'s codec does: one frame in, at
    /// most one frame out, and a malformed frame still answered because a
    /// client that is waiting has to be released.
    pub async fn frame(&self, raw: &str, out: &Arc<dyn FrameSink>) -> Option<String> {
        let response = self.answer(raw, out).await?;
        Some(serde_json::to_string(&response).unwrap_or_else(|error| {
            let internal = Response {
                jsonrpc: V2,
                id: response.id.clone(),
                payload: Payload::Error(RpcError::new(ErrorCode::Internal, error.to_string())),
            };
            serde_json::to_string(&internal).expect("an error object always serializes")
        }))
    }

    async fn answer(&self, raw: &str, out: &Arc<dyn FrameSink>) -> Option<Response> {
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(error) => return Some(refusal(ErrorCode::ParseError, error.to_string())),
        };

        match serde_json::from_value::<Message>(value) {
            Ok(Message::Notification(notification)) => {
                self.notified(notification).await;
                None
            }
            // A response answers a request this side made. Routing it to the
            // waiter is the whole of `session/request_permission` working:
            // dropping it would leave that request hanging until the
            // connection died.
            Ok(Message::Response(response)) => {
                self.settle(response);
                None
            }
            Ok(Message::Request(request)) => Some(self.dispatch(request, out).await),
            Err(error) => Some(refusal(ErrorCode::InvalidRequest, error.to_string())),
        }
    }

    async fn notified(&self, notification: Notification) {
        // ACP defines one client notification. An unknown one is ignored
        // rather than refused, so a client may speak a later minor version.
        if notification.method != method::SESSION_CANCEL {
            return;
        }
        let params = notification.params.unwrap_or_else(|| serde_json::json!({}));
        let Ok(params) = serde_json::from_value::<CancelNotification>(params) else {
            return;
        };
        self.cancel(&params.session_id).await;
    }

    /// Mark the prompt cancelled and ask the turn to stop.
    ///
    /// Both, in that order. The mark is what makes the prompt settle
    /// `cancelled` even if the turn was already finishing when the notification
    /// arrived - explicit cancellation outranks the turn's own reason, because
    /// the client asked and deserves to be told its ask was heard.
    ///
    /// An unknown session id is a no-op: ACP says so, and a client racing its
    /// own teardown should not be answered with an error it cannot use.
    async fn cancel(&self, session_id: &str) {
        let known = {
            let mut state = self.state.lock().expect("state");
            match state.sessions.get_mut(session_id) {
                Some(record) => {
                    record.cancelled = true;
                    true
                }
                None => false,
            }
        };
        if !known {
            return;
        }
        let _ = self
            .engine
            .agent_interrupt(SessionRef {
                session_id: session_id.to_string(),
            })
            .await;
    }

    fn settle(&self, response: Response) {
        let Id::Text(id) = &response.id else {
            // Every id this side mints is text. A numeric one answers a
            // request this side did not make.
            return;
        };
        let Some(waiter) = self.pending.lock().expect("pending").remove(id) else {
            return;
        };
        let _ = waiter.send(match response.payload {
            Payload::Result(value) => Ok(value),
            Payload::Error(error) => Err(error),
        });
    }

    async fn dispatch(&self, request: Request, out: &Arc<dyn FrameSink>) -> Response {
        let id = request.id.clone();
        match self.call(request, out).await {
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
        out: &Arc<dyn FrameSink>,
    ) -> Result<serde_json::Value, RpcError> {
        let params = request.params.unwrap_or_else(|| serde_json::json!({}));
        let name = request.method.as_str();

        if name != method::INITIALIZE {
            self.ready()?;
        }

        match name {
            method::INITIALIZE => encode(self.initialize(typed(params)?)),
            // No authentication method is advertised, so there is nothing to
            // authenticate with and nothing to check. A refusal here would fail
            // a client that is being polite.
            method::AUTHENTICATE => Ok(serde_json::json!({})),
            method::SESSION_NEW => encode(self.new_session(typed(params)?).await?),
            method::SESSION_PROMPT => encode(self.prompt(typed(params)?, out).await?),
            unknown => Err(RpcError::new(
                ErrorCode::MethodNotFound,
                format!("no method `{unknown}`"),
            )
            .with_data(serde_json::json!({ "method": unknown }))),
        }
    }

    fn initialize(&self, request: InitializeRequest) -> InitializeResponse {
        let _ = request.protocol_version;
        self.state.lock().expect("state").initialized = true;
        InitializeResponse {
            // A single-version agent answers its own version whatever it was
            // asked for. A client that cannot speak it will say so; a client
            // that can, can.
            protocol_version: PROTOCOL_VERSION,
            agent_info: AgentInfo {
                name: AGENT_NAME.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            agent_capabilities: AgentCapabilities {
                prompt_capabilities: PromptCapabilities {
                    image: false,
                    audio: false,
                    embedded_context: false,
                },
            },
            auth_methods: Vec::new(),
        }
    }

    async fn new_session(
        &self,
        request: NewSessionRequest,
    ) -> Result<NewSessionResponse, RpcError> {
        // ACP requires an absolute `cwd`. Checked rather than trusted because
        // a relative one resolves against *this* process's directory, which is
        // not the one the client meant, and the mistake is invisible until a
        // tool reads the wrong file.
        if request.cwd.is_empty() || !std::path::Path::new(&request.cwd).is_absolute() {
            return Err(
                RpcError::new(ErrorCode::InvalidParams, "`cwd` is an absolute path")
                    .with_data(serde_json::json!({ "field": "cwd" })),
            );
        }
        if !request.mcp_servers.is_empty() {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "this agent mounts no MCP servers",
            )
            .with_data(serde_json::json!({ "field": "mcpServers" })));
        }

        let info = self
            .engine
            .session_create(SessionCreateParams::default())
            .await?;
        self.state.lock().expect("state").sessions.insert(
            info.session_id.clone(),
            Record {
                inflight: false,
                cancelled: false,
            },
        );
        Ok(NewSessionResponse {
            // The engine's own id, not a second one minted here. One id means
            // an operator holding an ACP session id can find its journal, and
            // means there is no mapping table to fall out of step.
            session_id: info.session_id,
        })
    }

    async fn prompt(
        &self,
        request: PromptRequest,
        out: &Arc<dyn FrameSink>,
    ) -> Result<PromptResponse, RpcError> {
        let content = admit(&request.prompt).map_err(RpcError::from)?;
        self.claim(&request.session_id)?;

        let answered = self.run(&request.session_id, content, out).await;

        // The slot is released whichever way the turn went, and the
        // cancellation flag is read at the same moment, so a `session/cancel`
        // that arrived at any point during the turn is honoured exactly once.
        let cancelled = {
            let mut state = self.state.lock().expect("state");
            match state.sessions.get_mut(&request.session_id) {
                Some(record) => {
                    record.inflight = false;
                    std::mem::replace(&mut record.cancelled, false)
                }
                None => false,
            }
        };

        match answered {
            // Explicit cancellation outranks the turn's own reason. The client
            // asked for the turn to stop; whether the turn happened to reach a
            // natural end first is not the answer to that ask.
            _ if cancelled => Ok(PromptResponse {
                stop_reason: StopReason::Cancelled,
            }),
            Ok(reason) => Ok(PromptResponse {
                stop_reason: reason,
            }),
            // The engine's own word for an interrupted turn, reached when the
            // interrupt came from somewhere other than this connection.
            Err(error) if error.kind() == Some(ErrorCode::Cancelled) => Ok(PromptResponse {
                stop_reason: StopReason::Cancelled,
            }),
            Err(error) => Err(error),
        }
    }

    /// Subscribe, prompt, unsubscribe. The subscription opens first because the
    /// engine pushes on the thread that appends: one opened after the prompt
    /// would already have missed the turn's first events.
    async fn run(
        &self,
        session_id: &str,
        content: String,
        out: &Arc<dyn FrameSink>,
    ) -> Result<StopReason, RpcError> {
        let sink: Arc<dyn EventSink> = Arc::new(Updates {
            session_id: session_id.to_string(),
            out: Arc::clone(out),
        });
        let subscription = self
            .engine
            .session_subscribe(
                SessionSubscribeParams {
                    session_id: session_id.to_string(),
                    from_seq: None,
                },
                sink,
            )
            .await?;

        let ran = self
            .engine
            .agent_prompt(AgentPromptParams {
                session_id: session_id.to_string(),
                content,
            })
            .await;

        // Closed on both paths, and its failure never masks the turn's: a
        // client told "could not unsubscribe" would never learn why its prompt
        // failed.
        let _ = self
            .engine
            .session_unsubscribe(SessionUnsubscribeParams {
                subscription_id: subscription.subscription_id,
            })
            .await;

        Ok(StopReason::of(&ran?.summary.stop_reason))
    }

    /// Take the session's one prompt slot, or refuse.
    fn claim(&self, session_id: &str) -> Result<(), RpcError> {
        let mut state = self.state.lock().expect("state");
        let Some(record) = state.sessions.get_mut(session_id) else {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                format!("unknown session: {session_id}"),
            )
            .with_data(serde_json::json!({ "field": "sessionId" })));
        };
        if record.inflight {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "a prompt is already in flight for this session",
            ));
        }
        record.inflight = true;
        record.cancelled = false;
        Ok(())
    }

    /// Put one approval question to the client and wait for its answer.
    ///
    /// Every way of not producing a grant denies, which is contract section
    /// 4.4.7's rule and this bridge's too: a JSON-RPC error, a withdrawal, an
    /// option this agent never offered, or a connection that went away all
    /// settle as something other than [`ApprovalOutcome::AllowedOnce`].
    ///
    /// An option the agent did not offer is the one worth spelling out. A
    /// client answering `always-allow` to a question that offered two one-shot
    /// choices has either misunderstood or is a different implementation; in
    /// neither case is it a grant this bridge can honour, so it is
    /// [`ApprovalOutcome::Unavailable`] rather than a guess in the client's
    /// favour.
    pub async fn request_permission(
        &self,
        session_id: &str,
        tool_call_id: &str,
        out: &Arc<dyn FrameSink>,
    ) -> ApprovalOutcome {
        let id = format!(
            "tetanus-acp-{}",
            self.requests.fetch_add(1, Ordering::Relaxed)
        );
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending")
            .insert(id.clone(), sender);

        let request = permission_request(&id, session_id, tool_call_id);
        out.send_frame(serde_json::to_string(&request).expect("a frame serializes"));

        match receiver.await {
            Ok(answered) => outcome_of(answered),
            // The sender was dropped: the connection is closing. Nobody will
            // answer, so nobody granted.
            Err(_) => {
                self.pending.lock().expect("pending").remove(&id);
                ApprovalOutcome::Unavailable
            }
        }
    }

    /// Release the connection: refuse further work and free every waiter.
    pub async fn shutdown(&self) {
        {
            let mut state = self.state.lock().expect("state");
            state.closed = true;
            state.sessions.clear();
        }
        // Dropping the senders resolves every pending permission question as
        // unanswerable, which is what it is: there is no longer a client.
        self.pending.lock().expect("pending").clear();
    }

    fn ready(&self) -> Result<(), RpcError> {
        let state = self.state.lock().expect("state");
        if state.closed {
            return Err(RpcError::new(
                ErrorCode::Internal,
                "this ACP bridge has been shut down",
            ));
        }
        if !state.initialized {
            return Err(RpcError::new(
                ErrorCode::InvalidRequest,
                format!("`{}` is the first call on a connection", method::INITIALIZE),
            ));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl FrameHandler for AcpBridge {
    async fn frame(&self, raw: &str, out: &Arc<dyn FrameSink>) -> Option<String> {
        AcpBridge::frame(self, raw, out).await
    }

    async fn close(&self) {
        self.shutdown().await;
    }
}

/// The subscription sink for one prompt: journal events out as ACP updates.
struct Updates {
    session_id: String,
    out: Arc<dyn FrameSink>,
}

impl EventSink for Updates {
    fn session_event(&self, push: SessionEventPush) {
        for update in updates_of(&push.event) {
            let frame = Notification {
                jsonrpc: V2,
                method: agent::SESSION_UPDATE.to_string(),
                params: Some(
                    serde_json::to_value(SessionNotification {
                        session_id: self.session_id.clone(),
                        update,
                    })
                    .expect("an update serializes"),
                ),
            };
            self.out
                .send_frame(serde_json::to_string(&frame).expect("a frame serializes"));
        }
    }

    /// ACP has no word for the harness's live agent state, and inventing one
    /// would be this bridge extending someone else's protocol.
    fn agent_status(&self, _: AgentStatusPush) {}
}

/// The question, as a frame.
fn permission_request(id: &str, session_id: &str, tool_call_id: &str) -> Request {
    Request {
        jsonrpc: V2,
        // Text, and prefixed, so an id this side mints can never collide with
        // one the client is using for its own calls.
        id: Id::Text(id.to_string()),
        method: agent::REQUEST_PERMISSION.to_string(),
        params: Some(
            serde_json::to_value(RequestPermissionRequest {
                session_id: session_id.to_string(),
                tool_call: PermissionToolCall {
                    tool_call_id: tool_call_id.to_string(),
                },
                options: vec![
                    PermissionOption {
                        option_id: ALLOW_ONCE.to_string(),
                        name: "Allow once".to_string(),
                        kind: "allow_once".to_string(),
                    },
                    PermissionOption {
                        option_id: REJECT_ONCE.to_string(),
                        name: "Reject".to_string(),
                        kind: "reject_once".to_string(),
                    },
                ],
            })
            .expect("a request serializes"),
        ),
    }
}

/// The client's answer, read as a decision.
///
/// Every path that is not an explicit `allow-once` denies, and they are
/// gathered here so that is one readable list rather than a shape spread over
/// three nested matches: a JSON-RPC error, a body that is not an outcome, a
/// withdrawal, and an option this agent never offered.
fn outcome_of(answered: Result<serde_json::Value, RpcError>) -> ApprovalOutcome {
    let Ok(value) = answered else {
        return ApprovalOutcome::Unavailable;
    };
    let Ok(response) = serde_json::from_value::<RequestPermissionResponse>(value) else {
        return ApprovalOutcome::Unavailable;
    };
    match response.outcome {
        PermissionOutcome::Cancelled => ApprovalOutcome::Cancelled,
        PermissionOutcome::Selected { option_id } if option_id == ALLOW_ONCE => {
            ApprovalOutcome::AllowedOnce
        }
        PermissionOutcome::Selected { option_id } if option_id == REJECT_ONCE => {
            ApprovalOutcome::Rejected
        }
        PermissionOutcome::Selected { .. } => ApprovalOutcome::Unavailable,
    }
}

fn refusal(code: ErrorCode, message: impl Into<String>) -> Response {
    Response {
        jsonrpc: V2,
        id: Id::Null,
        payload: Payload::Error(RpcError::new(code, message)),
    }
}

fn typed<T: serde::de::DeserializeOwned>(params: serde_json::Value) -> Result<T, RpcError> {
    serde_json::from_value(params)
        .map_err(|error| RpcError::new(ErrorCode::InvalidParams, error.to_string()))
}

fn encode<T: serde::Serialize>(value: T) -> Result<serde_json::Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))
}
