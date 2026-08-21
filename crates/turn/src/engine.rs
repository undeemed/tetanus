//! The turn engine: the default driver of the documented dsh turn flow.
//!
//! ```text
//! turn/start
//!   claim next-step input plus one queued message
//!   -> agent/pre-step                   reject | enter(messages)
//!      reject, or a first enter rewritten empty -> close the turn with no step
//!      step/start
//!      append entered messages as user/message
//!      derive model history from the log
//!      assemble prompt sections + tool schemas   system-prompt/assemble
//!      agent/request -> llm/stream -> assistant/chunk* -> assistant/message
//!      tool/call* -> tools/pre-execute -> tools/execute -> tools/post-execute -> tool/result*
//!      step/end
//!      tools owe another request, or next-step input arrived -> claim -> next step
//!   -> agent/turn-stopping
//! turn/end
//! ```
//!
//! Source: upstream `docs/architecture.md` ("Turn flow") with the
//! `system-prompt/assemble` position taken from `docs/agent-lifecycle.md`,
//! which places it inside the step. See `docs/turn-flow.md` for the full
//! reconciliation.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::stream::{FuturesUnordered, StreamExt};
use futures_util::FutureExt;

use tetanus_core::events::Terminal;
use tetanus_core::{Context, EffectHandle, EventBus, ServiceError};
use tetanus_session::{SessionError, SessionLog};

use crate::approval::{ApprovalError, ApprovalPolicy, ApprovalRequest, ApprovalService};
use crate::boot::{LlmService, PromptService, SessionService, ToolsService};
use crate::compaction::{self, CompactionBudget, Summarizer};
use crate::events::{
    AgentRequest, AssemblePrompt, LlmStream, PreStep, PreStepDecision, RequestError,
    RequestErrorAction, RequestFailure, StopReason, SystemPrompt, ToolsExecute, ToolsPostExecute,
    ToolsPreExecute, TurnStopping, FAILED_STOP_REASON,
};
use crate::interrupt::Interrupt;
use crate::llm::{
    ChunkSink, LlmAdapter, LlmError, Message, ModelRequest, ModelResponse, StreamChunk,
};
use crate::log::{derive_messages, topic, with_system};
use crate::prompt::{AssembleAt, PromptError, PromptRegistry};
use crate::prune::PruneBudget;
use crate::tools::{
    Permission, ToolCall, ToolMode, ToolOrder, ToolOutcome, ToolRegistry, ToolSchema,
};

#[derive(Debug, thiserror::Error)]
pub enum TurnError {
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The assembly could not be rendered, so the step never asked anything.
    /// A section named a variable this assembly could not give it, which is a
    /// mistake in what a plugin registered rather than anything the model or
    /// the provider did.
    #[error(transparent)]
    Prompt(#[from] PromptError),
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Service(#[from] ServiceError),
    /// A listener that decides something panicked, and the turn was ended with
    /// it rather than the panic unwinding into whoever asked for the turn.
    ///
    /// The bus keeps `serial` and `waterfall` loud on purpose: a decision
    /// listener with a bug is not something to paper over, and a caller that
    /// asked one question should hear about it. What this contains is the
    /// *blast radius*. Before it, such a panic escaped `run_turn`, so
    /// `turn/start` was left unbalanced on the journal and the session was
    /// wedged open - a reader could not tell the turn was over, and the next
    /// open had to synthesize `interrupted` closers for a turn nothing
    /// interrupted. The panic is still reported, as this failure; the journal
    /// is closed the way section 4.4.2 says every failed turn closes.
    #[error("a plugin listener panicked: {0}")]
    Plugin(String),
    /// A decision about whether a tool may run could not be put at all.
    ///
    /// Not a denial: a denial is an outcome, and an outcome is a `tool/result`
    /// the model reads (contract section 4.4.7). This is the seam itself
    /// failing - a journal that refused the audit pair, or an ask attempted
    /// with no turn open - and a turn that cannot record what it decided must
    /// not proceed as though it had decided nothing.
    #[error(transparent)]
    Approval(#[from] ApprovalError),
}

#[derive(Debug, Clone)]
pub struct TurnConfig {
    pub model: String,
    /// Step budget for one turn. A model that never stops asking for tools ends
    /// the turn with [`StopReason::MaxSteps`] instead of running forever.
    pub max_steps: u32,
    pub max_tokens: Option<u32>,
    /// The engine's own system-prompt section, registered under
    /// [`prompt::BASE_SECTION`](crate::prompt::BASE_SECTION). Plugins add more
    /// through the registry, or rewrite the lot in `system-prompt/assemble`.
    pub base_prompt: String,
    /// How many parallel-safe tool calls of one step may be in flight at once.
    /// A cap of one is fully serial dispatch. It cannot be zero, because a pool
    /// that may start nothing never finishes.
    pub max_parallel_tool_calls: NonZeroUsize,
    /// The order the model is offered its tools in. `None` is canonical
    /// (lexicographic) order, which is what a harness that configured nothing
    /// gets; a [`ToolOrder`] was read against the registry it arranges.
    pub tool_order: Option<ToolOrder>,
    /// The deployment's answer for a session whose journal holds no
    /// `approval/policy`. Contract section 4.4.7: the journal's own switch wins
    /// over this, and this wins over nothing else.
    pub approval_policy: ApprovalPolicy,
    /// The routed model's context window, when the deployment knows it.
    ///
    /// Recorded on every `request/context`, so a reader of the journal can
    /// tell how close a request came to the limit without knowing the model's
    /// catalog. It is also what a compaction budget is scaled against.
    pub context_window: Option<u64>,
    /// What to do when the next request would not fit. `None` never compacts,
    /// which is what a deployment that has not set a window gets: a budget
    /// with no window to scale against is a guess, and a guess that silently
    /// rewrote a user's history would be the wrong kind of helpful.
    pub compaction: Option<AutoCompaction>,
}

/// The compaction a turn performs for itself, before a request it can already
/// see will not fit.
///
/// Doing it here rather than after a provider refusal is the whole point: a
/// refused request costs a round trip, and a `CONTEXT_WINDOW_EXCEEDED` is
/// terminal, so a turn that waited to be told would simply fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoCompaction {
    /// When to act, and how much recent conversation to keep verbatim.
    pub budget: CompactionBudget,
    /// Shrink over-long tool results first, when set. It is model-free and
    /// costs nothing, so it is worth trying before a summary that costs a
    /// provider call - and it often makes the summary unnecessary.
    pub prune: Option<PruneBudget>,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self {
            model: crate::llm::mock::MODEL.to_string(),
            max_steps: 8,
            max_tokens: None,
            base_prompt:
                "You are tetanus, a headless coding agent. Answer with the tools you have."
                    .to_string(),
            max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
            tool_order: None,
            approval_policy: ApprovalPolicy::Ask,
            context_window: None,
            compaction: None,
        }
    }
}

/// The default parallel cap, as upstream's `DEFAULT_MAX_PARALLEL_TOOL_CALLS`.
pub const DEFAULT_MAX_PARALLEL_TOOL_CALLS: NonZeroUsize =
    NonZeroUsize::new(10).expect("ten is not zero");

#[derive(Debug, Clone, PartialEq)]
pub struct TurnOutcome {
    pub turn: u64,
    pub steps: u32,
    pub reason: StopReason,
    /// The last assistant message of the turn.
    pub content: String,
    /// Set when an `agent/turn-stopping` listener bailed.
    pub stop_veto: Option<String>,
}

/// What a turn that reached its own end has to say for itself.
///
/// Not a [`TurnOutcome`]: the turn number and the step count belong to the
/// caller that opened the turn, which holds them whether the steps ended or
/// failed.
struct Closing {
    reason: StopReason,
    content: String,
    stop_veto: Option<String>,
}

/// What the caller that opened a turn holds while its steps run, so the
/// closers it writes can report the turn whichever way the steps ended.
#[derive(Default)]
struct Progress {
    steps: u32,
    /// The step that has a `step/start` and no `step/end` yet.
    open_step: Option<u32>,
}

pub struct TurnEngine {
    llm: Arc<dyn LlmAdapter>,
    tools: Arc<ToolRegistry>,
    log: Arc<dyn SessionLog>,
    bus: EventBus,
    prompt: Arc<PromptRegistry>,
    /// The seam that decides whether a gated call may run, and audits it. One
    /// per engine, because the audit ids it mints must not collide and the
    /// journal it writes to is this session's.
    approvals: Arc<ApprovalService>,
    /// The engine's own section, held for as long as the engine: dropping the
    /// handle takes the base prompt back out of the registry.
    _base: EffectHandle,
    config: TurnConfig,
    turns: AtomicU64,
    /// Set by [`TurnEngine::cancel`], read at each step boundary. An
    /// in-flight provider call is never aborted: the turn closes the way any
    /// other turn closes, so the journal stays a record of what happened. A
    /// wait between attempts is the exception - it is cut short, because it
    /// is time the turn was spending on nothing.
    interrupt: Arc<Interrupt>,
}

impl TurnEngine {
    /// Resolve every component from the typed registry. Nothing here names a
    /// concrete adapter, tool set, or storage backend.
    pub fn from_context(ctx: &Context, config: TurnConfig) -> Result<Self, TurnError> {
        let log = ctx.services.require::<SessionService>()?;
        // A resumed journal already holds turns. Numbering continues after
        // them, so no two turns in one log ever share an id.
        let turns = log
            .events()
            .iter()
            .filter(|event| event.ty == topic::TURN_START)
            .count() as u64;
        let prompt = ctx.services.require::<PromptService>()?;
        let base = prompt.seed_base(config.base_prompt.clone());
        let approvals =
            ApprovalService::new(ctx.bus.clone(), Arc::clone(&log), config.approval_policy);
        Ok(Self {
            llm: ctx.services.require::<LlmService>()?,
            tools: ctx.services.require::<ToolsService>()?,
            log,
            bus: ctx.bus.clone(),
            prompt,
            approvals,
            _base: base,
            config,
            turns: AtomicU64::new(turns),
            interrupt: Interrupt::new(),
        })
    }

    /// Run this engine's turns on an interrupt somebody else also holds.
    ///
    /// One flag, two holders. A tool that waits on something unbounded - a
    /// question put to a person ([`crate::questions`]) - has to learn that the
    /// turn was interrupted, and a tool body is handed nothing but its
    /// arguments, so the seam it waits on must have been given the flag when it
    /// was composed. Passing it here rather than reaching into a built engine
    /// keeps the composition explicit: whoever wires the tool and the engine
    /// together is the one place that knows they are the same turn.
    ///
    /// The engine still clears the flag as each turn starts, so an interrupt
    /// that stopped nothing does not stop the turn that follows it.
    pub fn sharing_interrupt(mut self, interrupt: Arc<Interrupt>) -> Self {
        self.interrupt = interrupt;
        self
    }

    /// Bring the next request inside its budget, if a budget was configured
    /// and the request is over it.
    ///
    /// The cheap remedy first: shrinking over-long tool results needs no
    /// provider, costs nothing and is often enough. A summary is asked for
    /// only when the request is still over budget after that, because it costs
    /// a provider call and it loses detail that pruning keeps.
    ///
    /// A compaction that cannot help - because the whole surface is the recent
    /// tail, or because the summary came back no smaller - leaves the request
    /// as it is rather than failing the turn. The provider may still accept it,
    /// and a turn that refused to try would be strictly worse than one that
    /// asked and was told no.
    async fn fit_context(&self, envelope: u64) -> Result<(), TurnError> {
        let Some(policy) = self.config.compaction else {
            return Ok(());
        };
        if self.cost(envelope) < policy.budget.threshold_tokens {
            return Ok(());
        }

        if let Some(budget) = policy.prune {
            // A prune only ever appends, so the one failure it can meet is the
            // journal refusing a write, and that is a failed turn either way.
            if let Err(compaction::CompactionError::Log(error)) =
                compaction::prune_results(self.log.as_ref(), budget)
            {
                return Err(error.into());
            }
            if self.cost(envelope) < policy.budget.threshold_tokens {
                return Ok(());
            }
        }

        let summarizer = self.summarizer();
        match compaction::compact(
            self.log.as_ref(),
            summarizer.as_ref(),
            &self.config.base_prompt,
            policy.budget,
        )
        .await
        {
            Ok(_) => Ok(()),
            // A compaction that could not run is not a turn that must fail.
            // The reason is on the journal, on the `compaction/end` the
            // transaction wrote before it gave up.
            Err(compaction::CompactionError::Log(error)) => Err(error.into()),
            Err(_) => Ok(()),
        }
    }

    /// What the next request would cost: the envelope plus the surface.
    fn cost(&self, envelope: u64) -> u64 {
        envelope + crate::tokens::TokenSurface::of(&self.log.events()).total_tokens()
    }

    /// Who writes a checkpoint for this engine.
    ///
    /// The routed model, through the adapter the turn already holds, so the
    /// summary is written by something that has seen this kind of work. There
    /// is no second route to configure and no second credential to hold.
    fn summarizer(&self) -> Arc<dyn Summarizer> {
        Arc::new(compaction::LlmSummarizer::new(
            Arc::clone(&self.llm),
            self.config.model.clone(),
            self.config.max_tokens,
        ))
    }

    /// Ask the running turn to stop at its next step boundary. Answering
    /// `false` means there was nothing to ask: no turn is running.
    pub fn cancel(&self) -> bool {
        self.interrupt.stop()
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn log(&self) -> &Arc<dyn SessionLog> {
        &self.log
    }

    /// The decision seam this engine gates its tool calls on.
    ///
    /// Published so a surface can read the session's policy and switch it
    /// (`approval.set`, contract section 4.4.7) against the same service the
    /// gate consults - two services would be two policies, and the one a
    /// caller set would not be the one the gate read.
    pub fn approvals(&self) -> &Arc<ApprovalService> {
        &self.approvals
    }

    /// The durability barrier a caller awaits when it needs the journal on disk
    /// before it continues.
    pub async fn flush(&self) -> Result<(), TurnError> {
        self.bus
            .parallel(&tetanus_session::SessionFlush {
                session_id: self.log.id().to_string(),
            })
            .await;
        self.log.flush()?;
        Ok(())
    }

    /// Run one turn to completion.
    ///
    /// Whatever ends the turn closes it on the journal. A failure ends a turn
    /// as surely as the model settling does, so `turn/end` is written before
    /// the failure is returned, and a `turn/start` with no `turn/end` means a
    /// process that died - which is what [`crate::repair`] answers.
    /// `docs/interface-contract.md` section 4.4.2 states it as a promise of
    /// the boundary.
    pub async fn run_turn(&self, input: &str) -> Result<TurnOutcome, TurnError> {
        // An interrupt that arrived while the session was idle stops nothing;
        // it must not stop the turn that starts next.
        self.interrupt.clear();
        let turn = self.turns.fetch_add(1, Ordering::Relaxed) + 1;
        self.log
            .append(topic::TURN_START, serde_json::json!({ "turn": turn }))?;

        let mut progress = Progress::default();
        // `AssertUnwindSafe` is the deliberate part. A panic mid-turn leaves
        // `progress` describing what the turn had done when it stopped, which
        // is not a corrupted value but exactly the value the closers need: the
        // step it left open is the step that must be ended. Nothing else
        // crosses the boundary, and the engine is not reused for another turn
        // on the strength of it.
        let ran = match AssertUnwindSafe(self.run_steps(turn, input, &mut progress))
            .catch_unwind()
            .await
        {
            Ok(ran) => ran,
            Err(payload) => {
                let fault = crate::tools::panicked(payload);
                tracing::error!(turn, %fault, "a plugin listener panicked; ending the turn");
                Err(TurnError::Plugin(fault))
            }
        };
        let closed = self.close(turn, &progress, &ran);

        // The failure that ended the turn is the one the caller hears, even
        // when the closers could not be written either: a journal that refused
        // a closer is usually the journal that refused the step.
        let closing = ran?;
        closed?;
        Ok(TurnOutcome {
            turn,
            steps: progress.steps,
            reason: closing.reason,
            content: closing.content,
            stop_veto: closing.stop_veto,
        })
    }

    /// Write the closers the turn owes its journal: the step it left open,
    /// then the turn itself.
    ///
    /// The two are the pair `docs/interface-contract.md` section 4.4.4 gives
    /// crash repair, in that order and with those payloads, because a reader
    /// cannot tell the two journals apart and should not have to.
    fn close(
        &self,
        turn: u64,
        progress: &Progress,
        ran: &Result<Closing, TurnError>,
    ) -> Result<(), SessionError> {
        if let Some(step) = progress.open_step {
            self.log.append(
                topic::STEP_END,
                serde_json::json!({ "turn": turn, "step": step }),
            )?;
        }
        self.log.append(
            topic::TURN_END,
            serde_json::json!({
                "turn": turn,
                "steps": progress.steps,
                "stop_reason": match ran {
                    Ok(closing) => closing.reason.as_str(),
                    Err(_) => FAILED_STOP_REASON,
                },
                // A turn nobody held open, and a turn that never reached the
                // checkpoint where it could be held, both report no veto.
                "stop_veto": ran.as_ref().ok().and_then(|closing| closing.stop_veto.clone()),
            }),
        )?;
        Ok(())
    }

    /// The steps of one turn: everything between the two durable ends
    /// [`Self::run_turn`] writes.
    ///
    /// The step count is the caller's rather than part of the answer, because
    /// the closer reports how many steps a turn spent even when the last of
    /// them failed.
    async fn run_steps(
        &self,
        turn: u64,
        input: &str,
        progress: &mut Progress,
    ) -> Result<Closing, TurnError> {
        let mut claimed = vec![Message::user(input)];
        let mut reason = StopReason::Natural;
        let mut content = String::new();

        loop {
            let step = progress.steps + 1;

            let mut pre_step = PreStep {
                turn,
                step,
                messages: std::mem::take(&mut claimed),
            };
            let entered = match self.bus.waterfall(&mut pre_step, enter_claimed()).await {
                PreStepDecision::Reject(_) => {
                    reason = StopReason::PreStepRejected;
                    break;
                }
                // A first enter rewritten empty closes the turn with no step.
                PreStepDecision::Enter(messages) if messages.is_empty() && step == 1 => {
                    reason = StopReason::PreStepRejected;
                    break;
                }
                PreStepDecision::Enter(messages) => messages,
            };
            progress.steps = step;

            self.log.append(
                topic::STEP_START,
                serde_json::json!({ "turn": turn, "step": step }),
            )?;
            progress.open_step = Some(step);
            for message in &entered {
                self.log.append(
                    topic::USER_MESSAGE,
                    serde_json::json!({ "content": message.content }),
                )?;
            }

            let at = AssembleAt { turn, step };
            let sections = self.prompt.assemble(&at);
            // A section registered as the whole prompt is kept aside here and
            // restored below. The assembly still runs in full, so tool schemas
            // and every other contribution still resolve and every listener
            // still sees them, but what the model reads is that one section as
            // it was assembled.
            let complete = self
                .prompt
                .complete_id()
                .and_then(|id| sections.iter().find(|s| s.id == id).cloned());
            let mut assemble = AssemblePrompt {
                turn,
                step,
                sections,
                tools: self.offered_tools(),
                variables: self.prompt.variables(&at),
            };
            let mut prompt = self.bus.waterfall(&mut assemble, assemble_prompt()).await;
            if let Some(section) = complete {
                prompt.sections = vec![section];
            }
            let system = prompt.render()?;

            // The envelope is priced once, here, because compaction is a
            // decision about the whole request and not about the conversation
            // alone: a large tool catalog leaves less room for history.
            let envelope = crate::tokens::estimate_text_tokens(&system)
                + crate::tokens::estimate_tools(&prompt.tools);
            self.fit_context(envelope).await?;

            // Model history is derived from the log, never stored beside it -
            // and after a compaction that means the compacted history, from
            // the same records a replay would read.
            let history = derive_messages(&self.log.events());

            // The request envelope, on the journal, before the request is
            // sent. It is what `context.breakdown` anchors on, and a turn that
            // then failed still says what it tried to send.
            self.log.append(
                compaction::topic::REQUEST_CONTEXT,
                serde_json::json!({
                    "turn": turn,
                    "step": step,
                    "provider": self.llm.provider(),
                    "model": self.config.model,
                    "context_window": self.config.context_window,
                    "system_tokens": crate::tokens::estimate_text_tokens(&system),
                    "tools_tokens": crate::tokens::estimate_tools(&prompt.tools),
                }),
            )?;

            let mut request = AgentRequest {
                turn,
                step,
                request: ModelRequest {
                    provider: self.llm.provider().to_string(),
                    model: self.config.model.clone(),
                    messages: with_system(&system, history),
                    tools: prompt.tools.clone(),
                    max_tokens: self.config.max_tokens,
                },
            };
            let request = self.bus.waterfall(&mut request, build_request()).await;

            let chunks = Arc::new(Mutex::new(Vec::new()));
            let mut stream = LlmStream {
                request,
                sink: Box::new(LogSink {
                    log: Arc::clone(&self.log),
                    turn,
                    step,
                    seqs: Arc::clone(&chunks),
                }),
            };
            // A failed call is offered to `agent/request-error` before it ends
            // the turn. A listener may answer `Retry`, and the same request
            // goes out again; the chunks the failed attempt already streamed
            // stay on the journal, with the policy's own records between them
            // and the next attempt to say why a second stream follows.
            let response = loop {
                match self.bus.waterfall(&mut stream, self.call_provider()).await {
                    Ok(response) => break response,
                    Err(failed) => {
                        let mut recovery = RequestError {
                            turn,
                            step,
                            provider: stream.request.provider.clone(),
                            failure: RequestFailure::from(&failed),
                            interrupt: Arc::clone(&self.interrupt),
                        };
                        let action = self.bus.waterfall(&mut recovery, no_recovery()).await;
                        // An interrupt beats a retry: a caller who has just
                        // asked the turn to stop does not want it to wait and
                        // try again.
                        if action != Some(RequestErrorAction::Retry) || self.interrupt.stopped() {
                            return Err(failed.into());
                        }
                    }
                }
            };
            let source_event_seqs = chunks.lock().expect("chunk seqs").clone();

            // A completion the provider cut off at its output cap stopped
            // mid-write, so it can have stopped in the middle of a call's
            // arguments: what it asked for is not known. None of its calls is
            // dispatched, and none is written to the anchor either, because a
            // call no result ever answers is a message the next request could
            // not carry (contract section 4.4.2). What did arrive stays on the
            // chunks the anchor's sources name.
            let truncated = response.truncated();
            let asked: &[ToolCall] = if truncated { &[] } else { &response.tool_calls };

            self.log.append_with_sources(
                topic::ASSISTANT_MESSAGE,
                serde_json::json!({
                    "content": response.content,
                    "reasoning": response.reasoning,
                    "tool_calls": asked,
                    "finish_reason": response.finish_reason,
                    "usage": response.usage,
                }),
                source_event_seqs,
            )?;
            content = response.content.clone();

            self.run_tool_calls(turn, asked).await?;

            self.log.append(
                topic::STEP_END,
                serde_json::json!({ "turn": turn, "step": step }),
            )?;
            progress.open_step = None;

            // The answer is incomplete and no tool result is owed, so there
            // is nothing a next step could carry.
            if truncated {
                reason = StopReason::MaxTokens;
                break;
            }
            // Tools owe another request -> claim -> next step. Phase ① has one
            // inbox holding one turn's input, so nothing new is claimed here.
            if response.tool_calls.is_empty() {
                break;
            }
            // The step boundary is where an interrupt lands. A turn that was
            // finished anyway is not reported as cancelled.
            if self.interrupt.stopped() {
                reason = StopReason::Cancelled;
                break;
            }
            if progress.steps >= self.config.max_steps {
                reason = StopReason::MaxSteps;
                break;
            }
        }

        // The terminal checkpoint runs only for a turn that spent a step; a
        // rejected first claim closes a durable turn without one.
        let stop_veto = if progress.steps > 0 {
            self.bus
                .serial(&TurnStopping {
                    turn,
                    steps: progress.steps,
                    reason,
                })
                .await
                .map(|veto| veto.reason)
        } else {
            None
        };

        Ok(Closing {
            reason,
            content,
            stop_veto,
        })
    }

    /// The schemas one assembly starts from, in the order the model reads them:
    /// canonical unless the harness settled a [`ToolOrder`]. The
    /// `system-prompt/assemble` waterfall still runs after this, and what a
    /// listener adds there is that listener's to order.
    fn offered_tools(&self) -> Vec<ToolSchema> {
        match &self.config.tool_order {
            Some(order) => self.tools.schemas_in(order),
            None => self.tools.schemas(),
        }
    }

    /// Run one step's tool calls: an exclusive call is a barrier, parallel-safe
    /// siblings share a bounded pool, and every result commits in model order.
    ///
    /// Parity: upstream `packages/core/agent-loop/src/tool-calls.ts`, pinned by
    /// its `tool-calls.spec.ts`.
    async fn run_tool_calls(&self, turn: u64, calls: &[ToolCall]) -> Result<(), TurnError> {
        let mut next = 0;
        while next < calls.len() {
            // Classify the head of the rest, not the whole list: a call is
            // classified as late as possible, just before it could start.
            let group = match self.tools.mode(&calls[next]) {
                ToolMode::Exclusive => &calls[next..next + 1],
                ToolMode::Parallel => &calls[next..],
            };
            next += self.run_tool_group(turn, group).await?;
        }
        Ok(())
    }

    /// Run one barrier or one parallel pool.
    ///
    /// Returns how many calls it consumed. A call the pool reclassifies as
    /// exclusive before starting it is not consumed: the pool drains, and that
    /// call opens the next group as its own barrier.
    ///
    /// Dispatch may overlap; commitment may not. A result is appended only when
    /// every earlier call of the step has been appended, so the journal reads
    /// in model order however the calls settled.
    async fn run_tool_group(&self, turn: u64, group: &[ToolCall]) -> Result<usize, TurnError> {
        let cap = self.config.max_parallel_tool_calls.get();
        let mut in_flight = FuturesUnordered::new();
        let mut settled: BTreeMap<usize, Settled> = BTreeMap::new();
        let mut started = 0;
        let mut committed = 0;
        // The first fault stops new dispatches. The calls already in flight are
        // still drained, because a started call has already had its effect.
        let mut failure: Option<TurnError> = None;

        loop {
            while failure.is_none() && started < group.len() && in_flight.len() < cap {
                if started > 0 && self.tools.mode(&group[started]) == ToolMode::Exclusive {
                    break;
                }
                in_flight.push(self.dispatch_tool_call(turn, started, &group[started]));
                started += 1;
            }
            let Some(dispatched) = in_flight.next().await else {
                break;
            };
            match dispatched {
                Ok((index, outcome)) => {
                    settled.insert(index, outcome);
                }
                Err(error) => failure = Some(failure.unwrap_or(error)),
            }
            while failure.is_none() {
                let Some(ready) = settled.remove(&committed) else {
                    break;
                };
                if let Err(error) = self.commit_tool_result(ready) {
                    failure = Some(error);
                }
                committed += 1;
            }
        }

        match failure {
            Some(error) => Err(error),
            None => Ok(started),
        }
    }

    /// Take one call through the documented pipeline, up to but not including
    /// its `tool/result`: that is the caller's, to append in model order.
    async fn dispatch_tool_call(
        &self,
        turn: u64,
        index: usize,
        call: &ToolCall,
    ) -> Result<(usize, Settled), TurnError> {
        let logged = self.log.append(
            topic::TOOL_CALL,
            serde_json::json!({
                "id": call.id,
                "name": call.name,
                "arguments": call.arguments,
            }),
        )?;

        let mut pre = ToolsPreExecute {
            turn,
            call: call.clone(),
        };
        let call = self.bus.waterfall(&mut pre, pass_call()).await;

        // The gate is here, after `tools/pre-execute` and before anything
        // runs. After, because a listener may rewrite the call and what is
        // decided must be what would actually run - approving one command and
        // executing another is the whole failure mode a gate exists to
        // prevent. Before, because a decision taken after the effect is not a
        // decision.
        if let Some(refusal) = self.decide(&call).await? {
            return Ok((
                index,
                Settled {
                    call,
                    outcome: ToolOutcome::failed(refusal),
                    call_seq: logged.seq,
                    code: Some(crate::approval::TOOL_NOT_PERMITTED),
                },
            ));
        }

        let mut execute = ToolsExecute {
            turn,
            call: call.clone(),
        };
        let outcome = match self.bus.waterfall(&mut execute, self.dispatch_tool()).await {
            Ok(outcome) => outcome,
            // A failed call is a binding rejection the model sees, not a turn
            // failure.
            Err(err) => ToolOutcome::failed(err.to_string()),
        };

        let mut post = ToolsPostExecute {
            turn,
            call: call.clone(),
            outcome,
        };
        let outcome = self.bus.waterfall(&mut post, pass_outcome()).await;

        Ok((
            index,
            Settled {
                call,
                outcome,
                call_seq: logged.seq,
                code: None,
            },
        ))
    }

    /// Put the question one call needs, and answer with the refusal the model
    /// should read - or `None` when the call may run.
    ///
    /// A call whose tool asks for nothing costs nothing: no question is put, no
    /// audit pair is written, and the journal of a session that never gates a
    /// call is byte-identical to the journal it had before this seam existed.
    ///
    /// Every way of not getting an answer denies, which is
    /// [`ApprovalService`]'s promise rather than this function's. What is
    /// decided here is only what the model is told, and that a denial is a
    /// result rather than a failure of the turn.
    async fn decide(&self, call: &ToolCall) -> Result<Option<String>, TurnError> {
        let Permission::Ask { reason } = self.tools.permission(call) else {
            return Ok(None);
        };
        let outcome = self
            .approvals
            .request(
                ApprovalRequest::new(&call.name)
                    .about_call(&call.id)
                    .because(reason),
                &self.interrupt,
            )
            .await?;
        Ok(outcome.refusal(&call.name))
    }

    /// Append one settled call's `tool/result`, citing the `tool/call` it
    /// answers.
    ///
    /// A result that carries a `code` is one nobody ran: contract section
    /// 4.3.2 fixes that meaning, and the field is absent - not null - on the
    /// results of calls that did run, so a reader tells the two apart by
    /// presence rather than by value.
    fn commit_tool_result(&self, settled: Settled) -> Result<(), TurnError> {
        let mut data = serde_json::json!({
            "call_id": settled.call.id,
            "name": settled.call.name,
            "ok": settled.outcome.ok,
            "content": settled.outcome.content,
        });
        if let Some(code) = settled.code {
            data["code"] = serde_json::json!(code);
        }
        self.log
            .append_with_sources(topic::TOOL_RESULT, data, vec![settled.call_seq])?;
        Ok(())
    }

    fn call_provider(&self) -> Terminal<LlmStream> {
        let adapter = Arc::clone(&self.llm);
        Arc::new(move |ev: &mut LlmStream| {
            let adapter = Arc::clone(&adapter);
            Box::pin(async move {
                let LlmStream { request, sink } = ev;
                adapter.stream(request, sink.as_mut()).await
            })
        })
    }

    fn dispatch_tool(&self) -> Terminal<ToolsExecute> {
        let tools = Arc::clone(&self.tools);
        Arc::new(move |ev: &mut ToolsExecute| {
            let tools = Arc::clone(&tools);
            Box::pin(async move { tools.execute(&ev.call).await })
        })
    }
}

/// One dispatched call, waiting for its turn to be committed.
struct Settled {
    /// The call as the pipeline left it: `tools/pre-execute` may have rewritten
    /// it, and the result must name what actually ran.
    call: ToolCall,
    outcome: ToolOutcome,
    /// The `tool/call` this result will cite.
    call_seq: u64,
    /// Set on a result nobody ran, saying why there is no outcome to report.
    /// `None` for every call that was actually dispatched.
    code: Option<&'static str>,
}

fn enter_claimed() -> Terminal<PreStep> {
    Arc::new(|ev: &mut PreStep| {
        Box::pin(async move { PreStepDecision::Enter(std::mem::take(&mut ev.messages)) })
    })
}

fn assemble_prompt() -> Terminal<AssemblePrompt> {
    Arc::new(|ev: &mut AssemblePrompt| {
        Box::pin(async move {
            SystemPrompt {
                sections: std::mem::take(&mut ev.sections),
                tools: std::mem::take(&mut ev.tools),
                variables: std::mem::take(&mut ev.variables),
            }
        })
    })
}

/// The built-in behaviour of `agent/request-error`: recover nothing, so the
/// failure the driver already has is the answer.
fn no_recovery() -> Terminal<RequestError> {
    Arc::new(|_ev: &mut RequestError| Box::pin(async move { None }))
}

fn build_request() -> Terminal<AgentRequest> {
    Arc::new(|ev: &mut AgentRequest| Box::pin(async move { ev.request.clone() }))
}

fn pass_call() -> Terminal<ToolsPreExecute> {
    Arc::new(|ev: &mut ToolsPreExecute| Box::pin(async move { ev.call.clone() }))
}

fn pass_outcome() -> Terminal<ToolsPostExecute> {
    Arc::new(|ev: &mut ToolsPostExecute| Box::pin(async move { ev.outcome.clone() }))
}

/// The sink the engine hands to the adapter: every chunk becomes a durable
/// `assistant/chunk`, so the log keeps the raw stream a replay or a UI needs.
struct LogSink {
    log: Arc<dyn SessionLog>,
    turn: u64,
    step: u32,
    seqs: Arc<Mutex<Vec<u64>>>,
}

#[async_trait::async_trait]
impl ChunkSink for LogSink {
    async fn chunk(&mut self, chunk: StreamChunk) -> Result<(), LlmError> {
        let mut data = serde_json::to_value(&chunk)
            .map_err(|e| LlmError::Sink(format!("chunk is not serializable: {e}")))?;
        if let Some(object) = data.as_object_mut() {
            object.insert("turn".into(), self.turn.into());
            object.insert("step".into(), self.step.into());
        }
        let event = self
            .log
            .append(topic::ASSISTANT_CHUNK, data)
            .map_err(|e| LlmError::Sink(e.to_string()))?;
        self.seqs.lock().expect("chunk seqs").push(event.seq);
        Ok(())
    }
}

/// Re-exported so a caller can build the same response shape a terminal returns.
pub type ProviderResult = Result<ModelResponse, LlmError>;
