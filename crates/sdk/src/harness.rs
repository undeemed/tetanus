//! The owned-run API: a session handle, and a turn that hands back everything
//! it produced.

use std::sync::Arc;

use tetanus_protocol::methods::{
    AgentPromptParams, AgentStatusPush, Engine, SessionCreateParams, SessionSubscribeParams,
};
use tetanus_protocol::types::{AgentState, SessionEvent, SessionInfo, TurnSummary};
use tetanus_query::Journal;

use crate::client::{Client, SdkError};
use crate::events::Update;

/// A client with a session-opening convenience on it.
///
/// Thin on purpose: everything here is two [`Client`] calls in the right
/// order, and the value is entirely in *which* order.
pub struct Harness {
    client: Client,
}

impl Harness {
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        Self {
            client: Client::new(engine),
        }
    }

    pub fn with_client(client: Client) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// A fresh session on the server's default route.
    pub async fn session(&self) -> Result<Session<'_>, SdkError> {
        self.open(SessionCreateParams::default()).await
    }

    /// A session on the caller's own terms - a named id, a route, a step cap.
    ///
    /// The handshake happens here rather than being demanded of the caller,
    /// because `start()` is memoized and a caller who has already called it
    /// pays nothing. That is the one place this layer is allowed to be
    /// implicit: [`Client`] stays explicit so a caller who wants to inspect the
    /// handshake still can.
    pub async fn open(&self, params: SessionCreateParams) -> Result<Session<'_>, SdkError> {
        self.client.start().await?;
        let info = self.client.session_create(params).await?;
        Ok(Session {
            client: &self.client,
            info,
        })
    }

    pub async fn close(&self) {
        self.client.close().await;
    }
}

/// One session, and the turns run on it.
pub struct Session<'a> {
    client: &'a Client,
    info: SessionInfo,
}

impl Session<'_> {
    pub fn id(&self) -> &str {
        &self.info.session_id
    }

    pub fn info(&self) -> &SessionInfo {
        &self.info
    }

    /// Run one turn and collect everything it produced.
    ///
    /// The order is the whole point. The subscription opens *before* the
    /// prompt, because the engine pushes on the thread that appends and a
    /// subscription opened afterwards has already missed `turn/start`. It
    /// closes afterwards whichever way the turn went, because a sink left open
    /// is one the engine keeps writing to.
    ///
    /// Collection is a drain rather than a wait: `agent.prompt` returns when
    /// the turn closes, and every push of that turn - the running status, each
    /// event, the idle status - was made before it returned. So by the time
    /// there is a summary to report, there is nothing left to arrive.
    pub async fn run(&self, content: impl Into<String>) -> Result<RunResult, SdkError> {
        let mut subscription = self
            .client
            .session_subscribe(SessionSubscribeParams {
                session_id: self.info.session_id.clone(),
                // Live only. A replay would hand back a previous turn's events
                // as if this turn had produced them.
                from_seq: None,
            })
            .await?;

        let ran = self
            .client
            .agent_prompt(AgentPromptParams {
                session_id: self.info.session_id.clone(),
                content: content.into(),
            })
            .await;

        let updates = subscription.drain();
        let subscription_id = subscription.id().to_string();
        // Closed on both paths, and its own failure never masks the turn's:
        // a caller told "could not unsubscribe" would never learn why the turn
        // failed.
        let _ = self.client.session_unsubscribe(&subscription_id).await;

        Ok(RunResult {
            session_id: self.info.session_id.clone(),
            summary: ran?.summary,
            updates,
        })
    }

    /// The whole journal, positioned - every turn, not just the last.
    pub async fn journal(&self) -> Result<Journal, SdkError> {
        self.client.journal(&self.info.session_id).await
    }

    pub async fn status(&self) -> Result<AgentState, SdkError> {
        Ok(self
            .client
            .agent_status(&self.info.session_id)
            .await?
            .status
            .state)
    }

    /// Ask the turn in flight to stop at its next step boundary.
    pub async fn interrupt(&self) -> Result<bool, SdkError> {
        Ok(self.client.agent_interrupt(&self.info.session_id).await?.ok)
    }
}

/// One turn's outcome, and everything the turn pushed while it ran.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub session_id: String,
    pub summary: TurnSummary,
    /// Both push kinds, interleaved, in the order the engine sent them.
    ///
    /// Kept as one list rather than split into two, because the relative order
    /// is a fact callers assert: the status goes to `running` before the
    /// turn's first event, and two lists could not say so.
    pub updates: Vec<Update>,
}

impl RunResult {
    /// The turn's answer. The last assistant message, as the summary reports
    /// it - not something reconstructed from the events, which could disagree.
    pub fn final_response(&self) -> &str {
        &self.summary.content
    }

    /// Just the journal events, in order.
    pub fn events(&self) -> Vec<SessionEvent> {
        self.updates
            .iter()
            .filter_map(|update| update.event().cloned())
            .collect()
    }

    /// Just the status transitions, in order.
    pub fn statuses(&self) -> Vec<AgentStatusPush> {
        self.updates
            .iter()
            .filter_map(|update| match update {
                Update::Status(status) => Some(status.clone()),
                Update::Event(_) => None,
            })
            .collect()
    }

    /// This turn's events as a journal, so the same questions that can be
    /// asked of a whole session can be asked of one turn.
    pub fn journal(&self) -> Journal {
        Journal::new(self.session_id.clone(), self.events())
    }
}
