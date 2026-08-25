//! The agent runtime: `agent.prompt`, `agent.status` and `agent.interrupt`.
//!
//! A prompt runs the documented turn flow on the session's own log and bus,
//! so every durable fact reaches subscribers as a `session/event` push while
//! the call is still open. The runtime adds only the one fact the journal
//! cannot carry: whether a turn is in flight. That is pushed as `agent/status`
//! on each transition, and read back by `agent.status`.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tetanus_protocol::methods::{
    Ack, AgentPromptParams, AgentPromptResult, AgentStatusPush, AgentStatusResult,
};
use tetanus_protocol::rpc::{ErrorCode, RpcError};
use tetanus_protocol::types::{AgentState, TurnSummary, Usage};
use tetanus_session::{SessionEvent, SessionLog};
use tetanus_turn::boot::{boot_with, PromptService};
use tetanus_turn::interrupt::Interrupt;
use tetanus_turn::llm::retry::{self, RetryPolicy};
use tetanus_turn::llm::{mock, LlmAdapter};
use tetanus_turn::log::topic;
use tetanus_turn::prompt::{PromptRegistry, Section};
use tetanus_turn::tools::{ToolOrder, ToolRegistry};
use tetanus_turn::{TurnConfig, TurnEngine, TurnError};

use crate::convert::{internal, stop_reason};
use crate::preset::Roster;
use crate::session::{LiveSession, SessionStore};
use crate::subscribe::Hub;

/// Resolves the provider a session's header names to the adapter that serves
/// it. A session records its provider when it is created, so the runtime does
/// not choose one, it asks. Adding a provider is then a boot-time change and
/// not an engine change.
pub trait Providers: Send + Sync {
    /// Every adapter this build can run, in the order a catalog lists them.
    fn all(&self) -> Vec<Arc<dyn LlmAdapter>>;

    /// The adapter for one provider route.
    fn adapter(&self, provider: &str) -> Option<Arc<dyn LlmAdapter>> {
        self.all()
            .into_iter()
            .find(|adapter| adapter.provider() == provider)
    }
}

/// Tools built for one session, against the interrupt that session's turns run
/// under.
///
/// A tool that starts a child process needs the same stop switch the loop
/// reads, and every session has a switch of its own: one registry shared
/// across sessions would mean interrupting one session killed another's
/// commands. A composition that has such tools supplies this; a composition
/// whose tools touch nothing outside the process leaves it unset and every
/// session shares [`EngineConfig::tools`](crate::EngineConfig::tools).
///
/// The registry it builds must hold the same tool names as `tools`, which is
/// what a catalog advertises and what a configured tool order was read
/// against.
pub type SessionTools =
    Arc<dyn Fn(&ToolScope<'_>, &Arc<Interrupt>) -> Arc<ToolRegistry> + Send + Sync>;

/// Which session a registry is being built for.
///
/// A tool that holds something outside the process needs to know whose it is.
/// A terminal session belongs to the session that opened it, and a spilled
/// build log belongs beside that session's journal: without a scope, a
/// composition can only guess, and both facts become "the current one" - which
/// is exactly the assumption that breaks the first time an engine serves two
/// sessions at once.
///
/// Borrowed rather than owned because it is read while the registry is being
/// built and never kept: a tool that needs the id past that point copies it.
/// `Debug` is written out rather than derived because a journal has no useful
/// rendering and deriving it would put the trait bound on every caller.
#[derive(Clone, Copy)]
pub struct ToolScope<'a> {
    /// The session's own id, as its journal header records it.
    pub session_id: &'a str,
    /// Where this session's durable artifacts already live - the directory of
    /// its journal - for a tool that has to put something on disk. `None` is a
    /// session with no file behind it, and a tool that needs one keeps nothing.
    pub artifacts: Option<&'a std::path::Path>,
    /// The journal this session's journal-backed tools fold over.
    ///
    /// The same log the turn engine appends to, deliberately: a feature tool
    /// keeps its whole state as a fold over the session's events, so a tool
    /// writing to a journal of its own would be state a replay could not
    /// reproduce and a reader could not find.
    pub log: &'a Arc<dyn SessionLog>,
}

impl std::fmt::Debug for ToolScope<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolScope")
            .field("session_id", &self.session_id)
            .field("artifacts", &self.artifacts)
            .finish_non_exhaustive()
    }
}

/// The offline default: the deterministic mock adapter and nothing else, so a
/// build with no configuration still runs a full turn with no key.
pub struct MockProviders;

impl Providers for MockProviders {
    fn all(&self) -> Vec<Arc<dyn LlmAdapter>> {
        vec![Arc::new(mock::MockAdapter::new())]
    }
}

/// One session's turn engine, kept between prompts because the engine numbers
/// the turns. It is built on first use: a session that is only ever listed
/// never boots a provider.
struct SessionAgent {
    engine: TurnEngine,
    /// True from the moment a prompt is accepted until its turn closes.
    busy: AtomicBool,
    /// The retry executor for this session's route. Declared before the
    /// context so it is removed first: it listens on the same bus the context
    /// unwinds.
    _retry: tetanus_core::EffectHandle,
    /// The persona this session's preset contributed, held for as long as the
    /// agent: dropping the handle takes the section back out of the registry.
    _persona: Option<tetanus_core::EffectHandle>,
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
    /// What a route with no block of its own does with a failed model
    /// request.
    retry: RetryPolicy,
    /// The routes whose provider wrote a block, and the policy it describes.
    /// A block is the whole policy for its route, so a route named here never
    /// reads `retry`.
    provider_retry: BTreeMap<String, RetryPolicy>,
    /// The order every turn on this engine offers its tools in, read against
    /// `tools` before the engine was built.
    tool_order: Option<ToolOrder>,
    /// How many parallel-safe tool calls of one step every turn on this engine
    /// may have in flight at once.
    max_parallel_tool_calls: NonZeroUsize,
    /// Builds this session's own tools when the composition has tools that
    /// need the session's interrupt; `None` shares `tools` with every session.
    session_tools: Option<SessionTools>,
    /// The named agents this engine composes. A session's header says which
    /// one it was composed from, and that is read once, when its turn engine
    /// is booted.
    presets: Roster,
    agents: Mutex<BTreeMap<String, Arc<SessionAgent>>>,
}

impl Runtime {
    /// Reads the runtime's share of a resolved engine configuration.
    ///
    /// It takes the whole document rather than a field per setting: every one
    /// of these is `EngineConfig`'s to decide, the list only grows, and a
    /// caller assembling eight positional arguments is a caller that can swap
    /// two of the same type without the compiler noticing.
    pub fn new(config: &crate::EngineConfig) -> Self {
        Self {
            providers: Arc::clone(&config.providers),
            tools: Arc::clone(&config.tools),
            retry: config.retry.clone(),
            provider_retry: config.provider_retry.clone(),
            tool_order: config.tool_order.clone(),
            max_parallel_tool_calls: config.max_parallel_tool_calls,
            session_tools: config.session_tools.clone(),
            presets: config.presets.clone(),
            agents: Mutex::new(BTreeMap::new()),
        }
    }

    /// The tools one session may call, and the prompt shape it opens with.
    ///
    /// A session composed from a preset that names a tool subset gets a
    /// registry holding those tools and no others - the model is never offered
    /// a tool it may not call, because being offered one and refused is a step
    /// spent on a refusal.
    ///
    /// The subset is taken against `base`, which is this session's own
    /// registry when the composition builds one per session: a preset names
    /// tools, and the tools it names are that session's, holding that
    /// session's stop switch.
    fn composed(
        &self,
        session: &LiveSession,
        base: &Arc<ToolRegistry>,
    ) -> Result<Composed, RpcError> {
        let Some(id) = session.header.preset.as_deref() else {
            return Ok(Composed::default());
        };
        // The header is the authority here, not the roster's default: this
        // session was composed from that id, whatever the document says now.
        let preset = self.presets.get(id).ok_or_else(|| {
            crate::convert::unknown_preset(crate::preset::PresetError::Unknown {
                id: id.to_string(),
                known: self.presets.ids(),
            })
        })?;
        let tools = match &preset.tools {
            None => None,
            Some(names) => Some(Arc::new(
                base.subset(names.iter().map(String::as_str))
                    .map_err(|refused| {
                        RpcError::new(
                            ErrorCode::InvalidParams,
                            format!("the preset {id:?} names {refused}"),
                        )
                        .with_data(serde_json::json!({ "field": "preset", "preset": id }))
                    })?,
            )),
        };
        Ok(Composed {
            tools,
            prompt: preset.prompt.clone(),
            persona: preset.persona.clone(),
        })
    }

    /// Run one turn and answer with its summary.
    ///
    /// The call returns when the turn closes. Its events do not wait for that:
    /// they are appends on the session's log, so a subscriber has already seen
    /// them by the time the summary arrives.
    pub async fn prompt(
        &self,
        sessions: &SessionStore,
        hub: &Hub,
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

        let before = session.log.events();
        let from_seq = before.len() as u64;
        let turn = turns_in(&before) + 1;
        hub.agent_status(status(
            &params.session_id,
            AgentState::Running,
            Some(turn),
            None,
        ));

        let started = Instant::now();
        let ran = agent.engine.run_turn(&params.content).await;
        // The journal is on disk before the summary is answered, so a surface
        // that reads the file next sees the turn the call just reported.
        let flushed = agent.engine.flush().await;
        drop(guard);
        hub.agent_status(status(&params.session_id, AgentState::Idle, None, None));

        let outcome = ran.map_err(|e| turn_error(&session, &e))?;
        flushed.map_err(|e| turn_error(&session, &e))?;

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

    /// The live state of one session. A surface that missed a push
    /// resynchronises here rather than folding the journal.
    pub fn status(
        &self,
        sessions: &SessionStore,
        session_id: &str,
    ) -> Result<AgentStatusResult, RpcError> {
        let session = sessions.open(session_id)?;
        Ok(AgentStatusResult {
            status: self.status_of(&session),
        })
    }

    fn status_of(&self, session: &LiveSession) -> AgentStatusPush {
        let id = &session.header.session_id;
        if !self.is_busy(id) {
            return status(id, AgentState::Idle, None, None);
        }
        // How far a running turn got is already on the journal: the last
        // `turn/start` and the last `step/start` are the progress.
        let events = session.log.events();
        status(
            id,
            AgentState::Running,
            last_number(&events, topic::TURN_START, "turn"),
            last_number(&events, topic::STEP_START, "step").map(|step| step as u32),
        )
    }

    /// Ask a running turn to stop at its next step boundary. A session with
    /// no turn in flight answers `ok: false`: there was nothing to stop, and
    /// two callers racing to interrupt one turn is not a fault.
    pub fn interrupt(&self, sessions: &SessionStore, session_id: &str) -> Result<Ack, RpcError> {
        let session = sessions.open(session_id)?;
        let id = &session.header.session_id;
        let agent = self.agents.lock().expect("agents").get(id).cloned();
        let asked =
            agent.is_some_and(|agent| agent.busy.load(Ordering::Acquire) && agent.engine.cancel());
        Ok(Ack { ok: asked })
    }

    /// Stop taking new work and close the turns already running, then answer
    /// what is still open.
    ///
    /// Contract section 4.4.11: a stopping server interrupts every running
    /// turn at the next step boundary - through the mechanism
    /// `agent.interrupt` already uses, not a second one - and waits for them
    /// to close, so a clean exit leaves nothing for section 4.4.4's repair to
    /// synthesize on the next open.
    ///
    /// **Best effort, with the crash path still behind it.** A tool that will
    /// not return cannot be waited for indefinitely, so the wait is bounded
    /// and a drain that runs out of time answers the turns it could not close.
    /// Those journals are exactly what repair exists for, which is why a
    /// deployment that sees `"interrupted"` after a restart is being told the
    /// drain did not finish.
    ///
    /// Answers the number of turns still running when the budget ran out;
    /// zero means every turn closed itself.
    pub async fn drain(&self, budget: Duration) -> usize {
        let running: Vec<Arc<SessionAgent>> = self
            .agents
            .lock()
            .expect("agents")
            .values()
            .filter(|agent| agent.busy.load(Ordering::Acquire))
            .cloned()
            .collect();
        for agent in &running {
            agent.engine.stop_for_shutdown();
        }

        // Polled rather than notified: a turn closes by returning from
        // `run_turn` on a task this engine does not own, so there is nothing
        // to await on. The interval is short enough that a drain is not
        // perceptibly slower than the turns it waits for.
        let deadline = Instant::now() + budget;
        loop {
            let open = running
                .iter()
                .filter(|agent| agent.busy.load(Ordering::Acquire))
                .count();
            if open == 0 {
                return 0;
            }
            if Instant::now() >= deadline {
                return open;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn is_busy(&self, session_id: &str) -> bool {
        self.agents
            .lock()
            .expect("agents")
            .get(session_id)
            .is_some_and(|agent| agent.busy.load(Ordering::Acquire))
    }

    /// The policy this route runs: its provider's own block when the document
    /// wrote one, and the general block otherwise.
    ///
    /// A block is the whole policy for its route rather than a patch on the
    /// general one, so the two are never layered here either - the same
    /// reason `retry::provider_policy` resolves a block over the compiled
    /// defaults. A route whose provider wrote `mode: always` would otherwise
    /// inherit a `max_retries` its author never wrote.
    fn policy_for(&self, provider: &str) -> RetryPolicy {
        self.provider_retry
            .get(provider)
            .cloned()
            .unwrap_or_else(|| self.retry.clone())
    }

    /// The tools a session may call, by name, after its preset has been
    /// applied. A surface that shows "this session's tools" reads this rather
    /// than the engine-wide catalogue, which is a superset for any session
    /// composed from a preset that narrows it.
    pub fn tools_for(&self, session: &LiveSession) -> Result<Vec<String>, RpcError> {
        // The engine-wide registry is the base here, not a session-built one:
        // a [`SessionTools`] builder must hold the same names as
        // [`EngineConfig::tools`], so the answer is the same and this costs no
        // child process for a question about names.
        let composed = self.composed(session, &self.tools)?;
        let registry = composed.tools.unwrap_or_else(|| Arc::clone(&self.tools));
        Ok(registry.names().cloned().collect())
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
        // One switch per session, shared by the loop and by any tool holding
        // work outside the process, so `agent.interrupt` on one session stops
        // that session's commands and nobody else's.
        let interrupt = Interrupt::new();
        let log = Arc::clone(&session.log) as Arc<dyn SessionLog>;
        let base = match &self.session_tools {
            Some(build) => build(
                &ToolScope {
                    session_id: &session.header.session_id,
                    artifacts: session.path.parent(),
                    log: &log,
                },
                &interrupt,
            ),
            None => Arc::clone(&self.tools),
        };
        let composed = self.composed(session, &base)?;
        let tools = composed.tools.clone().unwrap_or(base);
        let ctx =
            boot_with(session.bus.clone(), adapter, tools, log, interrupt).map_err(internal)?;

        // The persona is a prompt section rather than a rewritten base, so a
        // deployment's own words sit beside what plugins contribute instead of
        // replacing them. Order zero is where upstream puts it: after the
        // harness identity, before everything else.
        let persona = match &composed.persona {
            None => None,
            Some(text) => {
                let sections: Arc<PromptRegistry> =
                    ctx.services.require::<PromptService>().map_err(internal)?;
                Some(
                    sections
                        .section(Section::new(PERSONA_SECTION, PERSONA_ORDER, text.clone()))
                        .map_err(|refused| internal(refused.to_string()))?,
                )
            }
        };
        // The executor is scoped to the route the session named, which is
        // the route every request of this turn goes out on. A policy is a
        // provider's, not an engine's, so installing it per session is what
        // lets one document configure many routes later without moving this.
        let retry = retry::install(
            &session.bus,
            Arc::clone(&session.log) as Arc<dyn SessionLog>,
            session.header.provider.clone(),
            self.policy_for(&session.header.provider),
            retry::clock_jitter(),
        );
        let engine = TurnEngine::from_context(
            &ctx,
            TurnConfig {
                model: session.header.model.clone(),
                max_steps: session.header.max_steps,
                tool_order: self.tool_order.clone(),
                max_parallel_tool_calls: self.max_parallel_tool_calls,
                base_prompt: composed
                    .prompt
                    .clone()
                    .unwrap_or_else(|| TurnConfig::default().base_prompt),
                ..TurnConfig::default()
            },
        )
        .map_err(internal)?;

        let agent = Arc::new(SessionAgent {
            engine,
            busy: AtomicBool::new(false),
            _retry: retry,
            _persona: persona,
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

fn status(
    session_id: &str,
    state: AgentState,
    turn: Option<u64>,
    step: Option<u32>,
) -> AgentStatusPush {
    AgentStatusPush {
        session_id: session_id.to_string(),
        state,
        turn,
        step,
    }
}

fn turns_in(events: &[SessionEvent]) -> u64 {
    events
        .iter()
        .filter(|event| event.ty == topic::TURN_START)
        .count() as u64
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
fn turn_error(session: &LiveSession, error: &TurnError) -> RpcError {
    crate::convert::turn_error(
        &session.header.session_id,
        &session.header.provider,
        Some(&session.path),
        error,
    )
}

/// The name the persona section is registered under. Named, because a surface
/// that renders an assembled prompt shows section ids.
pub const PERSONA_SECTION: &str = "persona";

/// Where the persona sits: after the harness's own identity, before every
/// plugin contribution. Upstream puts its deployment persona at the same
/// order, and for the same reason - who the agent is comes before what it can
/// do.
pub const PERSONA_ORDER: i32 = 0;

/// What a preset contributes to one session's agent.
#[derive(Default)]
struct Composed {
    /// The tools the session may call, or `None` for every tool the engine
    /// has.
    tools: Option<Arc<ToolRegistry>>,
    /// The opening system-prompt section, or `None` for the engine's own.
    prompt: Option<String>,
    /// Who the agent is, as a section of its own.
    persona: Option<String>,
}
