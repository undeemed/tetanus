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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::stream::{FuturesUnordered, StreamExt};

use tetanus_core::events::Terminal;
use tetanus_core::{Context, EffectHandle, EventBus, ServiceError};
use tetanus_session::{SessionError, SessionLog};

use crate::boot::{LlmService, PromptService, SessionService, ToolsService};
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
use crate::prompt::{AssembleAt, PromptRegistry};
use crate::tools::{ToolCall, ToolMode, ToolOrder, ToolOutcome, ToolRegistry, ToolSchema};

#[derive(Debug, thiserror::Error)]
pub enum TurnError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Service(#[from] ServiceError),
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
        Ok(Self {
            llm: ctx.services.require::<LlmService>()?,
            tools: ctx.services.require::<ToolsService>()?,
            log,
            bus: ctx.bus.clone(),
            prompt,
            _base: base,
            config,
            turns: AtomicU64::new(turns),
            interrupt: Interrupt::new(),
        })
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
        let ran = self.run_steps(turn, input, &mut progress).await;
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

            // Model history is derived from the log, never stored beside it.
            let history = derive_messages(&self.log.events());

            let sections = self.prompt.assemble(&AssembleAt { turn, step });
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
            };
            let mut prompt = self.bus.waterfall(&mut assemble, assemble_prompt()).await;
            if let Some(section) = complete {
                prompt.sections = vec![section];
            }

            let mut request = AgentRequest {
                turn,
                step,
                request: ModelRequest {
                    provider: self.llm.provider().to_string(),
                    model: self.config.model.clone(),
                    messages: with_system(&prompt.text(), history),
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
            },
        ))
    }

    /// Append one settled call's `tool/result`, citing the `tool/call` it
    /// answers.
    fn commit_tool_result(&self, settled: Settled) -> Result<(), TurnError> {
        self.log.append_with_sources(
            topic::TOOL_RESULT,
            serde_json::json!({
                "call_id": settled.call.id,
                "name": settled.call.name,
                "ok": settled.outcome.ok,
                "content": settled.outcome.content,
            }),
            vec![settled.call_seq],
        )?;
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
