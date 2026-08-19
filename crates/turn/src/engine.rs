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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tetanus_core::events::Terminal;
use tetanus_core::{Context, EventBus, ServiceError};
use tetanus_session::{SessionError, SessionLog};

use crate::boot::{LlmService, SessionService, ToolsService};
use crate::events::{
    AgentRequest, AssemblePrompt, LlmStream, PreStep, PreStepDecision, PromptSection, StopReason,
    SystemPrompt, ToolsExecute, ToolsPostExecute, ToolsPreExecute, TurnStopping,
};
use crate::llm::{
    ChunkSink, LlmAdapter, LlmError, Message, ModelRequest, ModelResponse, StreamChunk,
};
use crate::log::{derive_messages, topic, with_system};
use crate::tools::{ToolCall, ToolOutcome, ToolRegistry};

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
    /// The base system-prompt section. Plugins add more through
    /// `system-prompt/assemble`.
    pub base_prompt: String,
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
        }
    }
}

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

pub struct TurnEngine {
    llm: Arc<dyn LlmAdapter>,
    tools: Arc<ToolRegistry>,
    log: Arc<dyn SessionLog>,
    bus: EventBus,
    config: TurnConfig,
    turns: AtomicU64,
}

impl TurnEngine {
    /// Resolve every component from the typed registry. Nothing here names a
    /// concrete adapter, tool set, or storage backend.
    pub fn from_context(ctx: &Context, config: TurnConfig) -> Result<Self, TurnError> {
        Ok(Self {
            llm: ctx.services.require::<LlmService>()?,
            tools: ctx.services.require::<ToolsService>()?,
            log: ctx.services.require::<SessionService>()?,
            bus: ctx.bus.clone(),
            config,
            turns: AtomicU64::new(0),
        })
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
    pub async fn run_turn(&self, input: &str) -> Result<TurnOutcome, TurnError> {
        let turn = self.turns.fetch_add(1, Ordering::Relaxed) + 1;
        self.log
            .append(topic::TURN_START, serde_json::json!({ "turn": turn }))?;

        let mut claimed = vec![Message::user(input)];
        let mut steps = 0u32;
        let mut reason = StopReason::Natural;
        let mut content = String::new();

        loop {
            let step = steps + 1;

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
            steps = step;

            self.log.append(
                topic::STEP_START,
                serde_json::json!({ "turn": turn, "step": step }),
            )?;
            for message in &entered {
                self.log.append(
                    topic::USER_MESSAGE,
                    serde_json::json!({ "content": message.content }),
                )?;
            }

            // Model history is derived from the log, never stored beside it.
            let history = derive_messages(&self.log.events());

            let mut assemble = AssemblePrompt {
                turn,
                step,
                sections: vec![PromptSection {
                    id: "base".into(),
                    text: self.config.base_prompt.clone(),
                }],
                tools: self.tools.schemas(),
            };
            let prompt = self.bus.waterfall(&mut assemble, assemble_prompt()).await;

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
            let response = self
                .bus
                .waterfall(&mut stream, self.call_provider())
                .await?;
            let source_event_seqs = chunks.lock().expect("chunk seqs").clone();

            self.log.append_with_sources(
                topic::ASSISTANT_MESSAGE,
                serde_json::json!({
                    "content": response.content,
                    "reasoning": response.reasoning,
                    "tool_calls": response.tool_calls,
                    "finish_reason": response.finish_reason,
                    "usage": response.usage,
                }),
                source_event_seqs,
            )?;
            content = response.content.clone();

            for call in &response.tool_calls {
                self.run_tool_call(turn, call).await?;
            }

            self.log.append(
                topic::STEP_END,
                serde_json::json!({ "turn": turn, "step": step }),
            )?;

            // Tools owe another request -> claim -> next step. Phase ① has one
            // inbox holding one turn's input, so nothing new is claimed here.
            if response.tool_calls.is_empty() {
                break;
            }
            if steps >= self.config.max_steps {
                reason = StopReason::MaxSteps;
                break;
            }
        }

        // The terminal checkpoint runs only for a turn that spent a step; a
        // rejected first claim closes a durable turn without one.
        let stop_veto = if steps > 0 {
            self.bus
                .serial(&TurnStopping {
                    turn,
                    steps,
                    reason,
                })
                .await
                .map(|veto| veto.reason)
        } else {
            None
        };

        self.log.append(
            topic::TURN_END,
            serde_json::json!({
                "turn": turn,
                "steps": steps,
                "stop_reason": reason.as_str(),
                "stop_veto": stop_veto,
            }),
        )?;

        Ok(TurnOutcome {
            turn,
            steps,
            reason,
            content,
            stop_veto,
        })
    }

    async fn run_tool_call(&self, turn: u64, call: &ToolCall) -> Result<(), TurnError> {
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

        self.log.append_with_sources(
            topic::TOOL_RESULT,
            serde_json::json!({
                "call_id": call.id,
                "name": call.name,
                "ok": outcome.ok,
                "content": outcome.content,
            }),
            vec![logged.seq],
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
