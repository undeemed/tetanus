//! The agent runtime: `agent.prompt`.
//!
//! A prompt runs the documented turn flow on the session's own log and bus,
//! so every durable fact reaches subscribers as a `session/event` push while
//! the call is still open.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tetanus_protocol::methods::{AgentPromptParams, AgentPromptResult};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types::{TurnSummary, Usage};
use tetanus_session::{SessionEvent, SessionLog};
use tetanus_turn::boot::boot;
use tetanus_turn::llm::{mock, LlmAdapter, LlmError};
use tetanus_turn::log::topic;
use tetanus_turn::tools::ToolRegistry;
use tetanus_turn::{TurnConfig, TurnEngine, TurnError};

use crate::convert::{internal, session_error, stop_reason};
use crate::session::{LiveSession, SessionStore};

/// Resolves the provider a session's header names to the adapter that serves
/// it. A session records its provider when it is created, so the runtime does
/// not choose one, it asks. Adding a provider is then a boot-time change and
/// not an engine change.
pub trait Providers: Send + Sync {
    fn adapter(&self, provider: &str) -> Option<Arc<dyn LlmAdapter>>;
}

/// The offline default: the deterministic mock adapter and nothing else, so a
/// build with no configuration still runs a full turn with no key.
pub struct MockProviders;

impl Providers for MockProviders {
    fn adapter(&self, provider: &str) -> Option<Arc<dyn LlmAdapter>> {
        (provider == mock::PROVIDER)
            .then(|| Arc::new(mock::MockAdapter::new()) as Arc<dyn LlmAdapter>)
    }
}

/// One session's turn engine, kept between prompts because the engine numbers
/// the turns. It is built on first use: a session that is only ever listed
/// never boots a provider.
struct SessionAgent {
    engine: TurnEngine,
    /// True from the moment a prompt is accepted until its turn closes.
    busy: AtomicBool,
    /// The boot context owns the plugin registrations. Dropping it would
    /// unwind them, so it lives exactly as long as the engine it built.
    _ctx: tetanus_core::Context,
}

/// Held for as long as a turn runs. Releasing on drop is what stops a turn
/// that failed, or panicked, from leaving the session busy for ever.
struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub struct Runtime {
    providers: Arc<dyn Providers>,
    tools: Arc<ToolRegistry>,
    agents: Mutex<BTreeMap<String, Arc<SessionAgent>>>,
}

impl Runtime {
    pub fn new(providers: Arc<dyn Providers>, tools: Arc<ToolRegistry>) -> Self {
        Self {
            providers,
            tools,
            agents: Mutex::new(BTreeMap::new()),
        }
    }

    /// Run one turn and answer with its summary.
    ///
    /// The call returns when the turn closes. Its events do not wait for that:
    /// they are appends on the session's log, so a subscriber has already seen
    /// them by the time the summary arrives.
    pub async fn prompt(
        &self,
        sessions: &SessionStore,
        params: AgentPromptParams,
    ) -> Result<AgentPromptResult, RpcError> {
        let session = sessions.open(&params.session_id)?;
        let agent = self.agent_for(&session)?;

        // One turn at a time. The claim is the compare-exchange, so two
        // prompts racing each other cannot both win.
        if agent
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(busy(&session));
        }
        let guard = BusyGuard(&agent.busy);

        let from_seq = session.log.events().len() as u64;
        let started = Instant::now();
        let ran = agent.engine.run_turn(&params.content).await;
        // The journal is on disk before the summary is answered, so a surface
        // that reads the file next sees the turn the call just reported.
        let flushed = agent.engine.flush().await;
        drop(guard);

        let outcome = ran.map_err(|e| turn_error(&session, e))?;
        flushed.map_err(|e| turn_error(&session, e))?;

        Ok(AgentPromptResult {
            summary: TurnSummary {
                turn: outcome.turn,
                steps: outcome.steps,
                stop_reason: stop_reason(outcome.reason),
                stop_veto: outcome.stop_veto,
                content: outcome.content,
                duration_ms: Some(started.elapsed().as_millis() as u64),
                usage: usage_since(&session.log.events(), from_seq),
            },
        })
    }

    /// The turn engine for one session, booted on first use against the
    /// provider and model its header names.
    fn agent_for(&self, session: &LiveSession) -> Result<Arc<SessionAgent>, RpcError> {
        let id = session.header.session_id.clone();
        if let Some(agent) = self.agents.lock().expect("agents").get(&id) {
            return Ok(Arc::clone(agent));
        }

        let adapter = self
            .providers
            .adapter(&session.header.provider)
            .ok_or_else(|| unknown_provider(&session.header.provider))?;
        let ctx = boot(
            session.bus.clone(),
            adapter,
            Arc::clone(&self.tools),
            Arc::clone(&session.log) as Arc<dyn SessionLog>,
        )
        .map_err(internal)?;
        let engine = TurnEngine::from_context(
            &ctx,
            TurnConfig {
                model: session.header.model.clone(),
                max_steps: session.header.max_steps,
                ..TurnConfig::default()
            },
        )
        .map_err(internal)?;

        let agent = Arc::new(SessionAgent {
            engine,
            busy: AtomicBool::new(false),
            _ctx: ctx,
        });
        // Another caller may have booted the same session meanwhile; the one
        // already in the map wins, so a session never has two turn engines.
        Ok(Arc::clone(
            self.agents
                .lock()
                .expect("agents")
                .entry(id)
                .or_insert(agent),
        ))
    }
}

fn last_number(events: &[SessionEvent], ty: &str, field: &str) -> Option<u64> {
    events
        .iter()
        .rev()
        .find(|event| event.ty == ty)?
        .data
        .get(field)?
        .as_u64()
}

/// Tokens the turn spent, summed over the `assistant/message` events it wrote.
/// `None` when no response carried usage: the contract reads `None` as
/// unmeasured, and zero would be a measurement.
fn usage_since(events: &[SessionEvent], from_seq: u64) -> Option<Usage> {
    let mut total: Option<Usage> = None;
    for event in events
        .iter()
        .filter(|event| event.seq >= from_seq && event.ty == topic::ASSISTANT_MESSAGE)
    {
        let Some(spent) = event
            .data
            .get("usage")
            .and_then(|usage| serde_json::from_value::<Usage>(usage.clone()).ok())
        else {
            continue;
        };
        let sum = total.get_or_insert_with(Usage::default);
        sum.prompt_tokens += spent.prompt_tokens;
        sum.completion_tokens += spent.completion_tokens;
    }
    total
}

fn busy(session: &LiveSession) -> RpcError {
    let id = &session.header.session_id;
    RpcError::new(
        ErrorCode::SessionBusy,
        format!("a turn is already running on session `{id}`"),
    )
    .with_data(serde_json::json!({
        "session_id": id,
        "turn": last_number(&session.log.events(), topic::TURN_START, "turn"),
    }))
}

/// A session whose header names a provider this build has no adapter for. The
/// faulty input is the `provider` the session was created with, which is why
/// it is `InvalidParams` and not an internal fault.
fn unknown_provider(provider: &str) -> RpcError {
    RpcError::new(
        ErrorCode::InvalidParams,
        format!("no adapter for provider `{provider}` in this build"),
    )
    .with_data(serde_json::json!({ "field": "provider", "provider": provider }))
}

/// Contract section 4.5. A turn that failed reports why in the provider's own
/// terms, because the surface's next move differs: a missing key is fixed by
/// the human, a provider error by retrying.
fn turn_error(session: &LiveSession, error: TurnError) -> RpcError {
    let provider = session.header.provider.clone();
    match error {
        TurnError::Session(e) => session_error(&session.header.session_id, e),
        TurnError::Service(e) => internal(e),
        TurnError::Llm(LlmError::MissingCredential(env) | LlmError::InvalidCredential(env)) => {
            RpcError::new(
                ErrorCode::MissingCredential,
                format!("provider `{provider}` has no usable credential at {env}"),
            )
            .with_data(serde_json::json!({ "provider": provider, "env": env }))
        }
        TurnError::Llm(LlmError::Provider { status, message }) => RpcError::new(
            ErrorCode::ProviderError,
            format!("provider `{provider}` answered {status}: {message}"),
        )
        .with_data(serde_json::json!({ "provider": provider, "status": status })),
        // No status: the provider never answered, so the field the table
        // names is absent rather than invented.
        TurnError::Llm(other) => RpcError::new(
            ErrorCode::ProviderError,
            format!("provider `{provider}` failed: {other}"),
        )
        .with_data(serde_json::json!({ "provider": provider })),
    }
}
