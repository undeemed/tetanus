//! The protocol client: one method per contract call, and nothing implicit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tetanus_protocol::methods::{
    method, Ack, AgentPromptParams, AgentPromptResult, AgentStatusResult, ApprovalSetParams,
    ConfigDumpResult, Engine, EventSink, HelloParams, HelloResult, ModelCatalogResult, PeerInfo,
    SessionCreateParams, SessionEventsParams, SessionEventsResult, SessionForkParams,
    SessionListResult, SessionRef, SessionSubscribeParams, SessionUnsubscribeParams,
    ToolCatalogResult,
};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types::SessionInfo;
use tetanus_protocol::PROTOCOL_VERSION;
use tetanus_query::Journal;

use crate::events::{Channel, Subscription};
use crate::CLIENT_NAME;

/// Why a call did not happen.
///
/// Three cases, and only one of them is the engine's. Keeping the other two
/// out of [`RpcError`] is the point: "you have not shaken hands yet" and "this
/// client is closed" are facts about the caller's own object, and a caller that
/// had to distinguish them by matching an error *code* would be parsing its own
/// mistakes out of a wire format.
#[derive(Debug, Clone, PartialEq)]
pub enum SdkError {
    /// The engine refused. Carried whole - code and `data` intact - because
    /// the contract's error table is the caller's documentation for it.
    Refused(RpcError),
    /// A call was made before [`Client::start`].
    NotStarted,
    /// A call was made after [`Client::close`].
    Closed,
}

impl std::fmt::Display for SdkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(error) => write!(f, "{} (code {})", error.message, error.code),
            Self::NotStarted => write!(f, "`{}` is the first call on a client", method::HELLO),
            Self::Closed => f.write_str("this client is closed"),
        }
    }
}

impl std::error::Error for SdkError {}

impl From<RpcError> for SdkError {
    fn from(error: RpcError) -> Self {
        Self::Refused(error)
    }
}

impl From<tetanus_query::QueryError> for SdkError {
    fn from(error: tetanus_query::QueryError) -> Self {
        Self::Refused(error.into())
    }
}

impl From<SdkError> for RpcError {
    fn from(error: SdkError) -> Self {
        match error {
            SdkError::Refused(error) => error,
            // The same code and the same words a carrier answers a call made
            // before the handshake with, so a caller that moves from this
            // client to a socket meets no new failure.
            other @ SdkError::NotStarted => {
                RpcError::new(ErrorCode::InvalidRequest, other.to_string())
            }
            other @ SdkError::Closed => RpcError::new(ErrorCode::InvalidRequest, other.to_string()),
        }
    }
}

/// The shared state behind a client, so a [`Subscription`] can close itself
/// without holding the client that made it.
pub(crate) struct Inner {
    engine: Arc<dyn Engine>,
    /// The handshake, once it has happened. `None` means it has not.
    server: Mutex<Option<HelloResult>>,
    closed: AtomicBool,
    /// Subscriptions this client opened and has not closed.
    open: Mutex<Vec<String>>,
}

impl Inner {
    pub(crate) async fn unsubscribe(&self, subscription_id: &str) -> Result<(), SdkError> {
        self.open
            .lock()
            .expect("open")
            .retain(|open| open != subscription_id);
        self.engine
            .session_unsubscribe(SessionUnsubscribeParams {
                subscription_id: subscription_id.to_string(),
            })
            .await?;
        Ok(())
    }
}

/// A typed client over one engine.
///
/// The handshake is enforced here rather than assumed, and that is deliberate:
/// contract section 4.4.1 makes `rpc.hello` the first call on a connection, a
/// carrier refuses anything before it, and an SDK that quietly skipped it would
/// let an in-process caller work against a version it never agreed on - and
/// then fail the day someone moved that caller onto a socket.
pub struct Client {
    inner: Arc<Inner>,
}

impl Client {
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        Self {
            inner: Arc::new(Inner {
                engine,
                server: Mutex::new(None),
                closed: AtomicBool::new(false),
                open: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Shake hands, at most once.
    ///
    /// Memoized on success, so a caller may call it before every operation
    /// without a second handshake happening. A *refused* handshake is not
    /// memoized: it leaves the client ungreeted so a caller may correct itself
    /// and try again, which is exactly what the codec does with a refused
    /// `rpc.hello` frame.
    pub async fn start(&self) -> Result<HelloResult, SdkError> {
        self.alive()?;
        if let Some(server) = self.inner.server.lock().expect("server").clone() {
            return Ok(server);
        }
        let server = self
            .inner
            .engine
            .hello(HelloParams {
                protocol_version: PROTOCOL_VERSION.to_string(),
                client: PeerInfo {
                    name: CLIENT_NAME.to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
            })
            .await?;
        *self.inner.server.lock().expect("server") = Some(server.clone());
        Ok(server)
    }

    /// What the handshake settled, or `None` before it has happened.
    pub fn server(&self) -> Option<HelloResult> {
        self.inner.server.lock().expect("server").clone()
    }

    /// Whether this build serves an optional call, by its `capability` string.
    ///
    /// A surface asks before it offers an affordance, rather than discovering
    /// the absence as an error mid-turn. Unshaken hands answer `false`: nothing
    /// has been promised yet.
    pub fn supports(&self, capability: &str) -> bool {
        self.server()
            .is_some_and(|server| server.capabilities.iter().any(|held| held == capability))
    }

    /// Close this client and everything it opened.
    ///
    /// Idempotent, and terminal: a closed client refuses every call. The
    /// subscriptions go with it for the reason a carrier closes a connection's
    /// subscriptions when its peer hangs up - a sink nobody reads is one the
    /// engine would otherwise push into for the life of the process.
    ///
    /// Failures closing a subscription are dropped: there is no longer anyone
    /// to report them to, and refusing to close would leave more behind than
    /// it saved.
    pub async fn close(&self) {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let open = std::mem::take(&mut *self.inner.open.lock().expect("open"));
        for subscription_id in open {
            let _ = self
                .inner
                .engine
                .session_unsubscribe(SessionUnsubscribeParams { subscription_id })
                .await;
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    /// The engine underneath, for a caller that needs the raw facade.
    pub fn engine(&self) -> &Arc<dyn Engine> {
        &self.inner.engine
    }

    // ---- the contract's calls, one method each ---------------------------

    pub async fn session_create(
        &self,
        params: SessionCreateParams,
    ) -> Result<SessionInfo, SdkError> {
        self.ready()?;
        Ok(self.inner.engine.session_create(params).await?)
    }

    pub async fn session_list(&self) -> Result<SessionListResult, SdkError> {
        self.ready()?;
        Ok(self.inner.engine.session_list().await?)
    }

    pub async fn session_events(
        &self,
        params: SessionEventsParams,
    ) -> Result<SessionEventsResult, SdkError> {
        self.ready()?;
        Ok(self.inner.engine.session_events(params).await?)
    }

    pub async fn session_fork(&self, params: SessionForkParams) -> Result<SessionInfo, SdkError> {
        self.ready()?;
        Ok(self.inner.engine.session_fork(params).await?)
    }

    /// Open a subscription and hand back its reading end.
    ///
    /// The sink is this crate's, so the caller receives typed updates rather
    /// than frames, and never learns that `EventSink` exists.
    pub async fn session_subscribe(
        &self,
        params: SessionSubscribeParams,
    ) -> Result<Subscription, SdkError> {
        self.ready()?;
        let (sender, updates) = tokio::sync::mpsc::unbounded_channel();
        let sink: Arc<dyn EventSink> = Arc::new(Channel(sender));
        let result = self.inner.engine.session_subscribe(params, sink).await?;
        self.inner
            .open
            .lock()
            .expect("open")
            .push(result.subscription_id.clone());
        Ok(Subscription {
            subscription_id: result.subscription_id,
            last_seq: result.last_seq,
            updates,
            client: Arc::clone(&self.inner),
        })
    }

    pub async fn session_unsubscribe(&self, subscription_id: &str) -> Result<(), SdkError> {
        self.ready()?;
        self.inner.unsubscribe(subscription_id).await
    }

    pub async fn agent_prompt(
        &self,
        params: AgentPromptParams,
    ) -> Result<AgentPromptResult, SdkError> {
        self.ready()?;
        Ok(self.inner.engine.agent_prompt(params).await?)
    }

    pub async fn agent_status(&self, session_id: &str) -> Result<AgentStatusResult, SdkError> {
        self.ready()?;
        Ok(self
            .inner
            .engine
            .agent_status(SessionRef {
                session_id: session_id.to_string(),
            })
            .await?)
    }

    pub async fn agent_interrupt(&self, session_id: &str) -> Result<Ack, SdkError> {
        self.ready()?;
        Ok(self
            .inner
            .engine
            .agent_interrupt(SessionRef {
                session_id: session_id.to_string(),
            })
            .await?)
    }

    pub async fn catalog_tools(&self) -> Result<ToolCatalogResult, SdkError> {
        self.ready()?;
        Ok(self.inner.engine.catalog_tools().await?)
    }

    pub async fn catalog_models(&self) -> Result<ModelCatalogResult, SdkError> {
        self.ready()?;
        Ok(self.inner.engine.catalog_models().await?)
    }

    pub async fn config_dump(&self) -> Result<ConfigDumpResult, SdkError> {
        self.ready()?;
        Ok(self.inner.engine.config_dump().await?)
    }

    /// A reserved call, routed rather than unknown. Contract section 4.2: a
    /// build that does not serve it answers `NotImplemented`, and this hands
    /// that answer through unchanged rather than hiding the call.
    pub async fn approval_set(&self, params: ApprovalSetParams) -> Result<Ack, SdkError> {
        self.ready()?;
        Ok(self.inner.engine.approval_set(params).await?)
    }

    // ---- reading a session as data ---------------------------------------

    /// The whole journal, positioned and ready to be asked questions.
    ///
    /// Here rather than left to the caller because loading one means paging
    /// `session.events` to `eof`, and a caller that stops at the first short
    /// page gets a truthful-looking answer about half a session.
    pub async fn journal(&self, session_id: &str) -> Result<Journal, SdkError> {
        self.ready()?;
        Ok(Journal::load(&self.inner.engine, session_id).await?)
    }

    fn alive(&self) -> Result<(), SdkError> {
        if self.is_closed() {
            return Err(SdkError::Closed);
        }
        Ok(())
    }

    /// Closed, then ungreeted. In that order: a closed client has not merely
    /// failed to shake hands, and reporting the weaker fault would send a
    /// caller off to call `start()` on something that will never work again.
    fn ready(&self) -> Result<(), SdkError> {
        self.alive()?;
        if self.inner.server.lock().expect("server").is_none() {
            return Err(SdkError::NotStarted);
        }
        Ok(())
    }
}
