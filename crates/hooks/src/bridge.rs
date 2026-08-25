//! Registering the hook protocol against a running turn.
//!
//! Every other module in this crate is pure: it decides which hooks match,
//! what is written to them, what they may write back, and how several answers
//! combine. None of it knows a turn is happening. This is the part that does —
//! it binds those decisions to `crates/turn`'s extension points, which is the
//! whole of what upstream calls a bridge.
//!
//! # The two tool points, and why they are one hook run
//!
//! `PreToolUse` has two jobs that land at different places in the pipeline. A
//! hook may **rewrite** the call, which belongs at `tools/pre-execute` because
//! that event's answer *is* the call that then runs; and it may **forbid** the
//! call, which belongs at `tools/permission`, the seam that can refuse one.
//! The pipeline runs them in that order, `tools/pre-execute` then
//! `tools/permission`, so that a decision is taken about what would actually
//! run rather than about what the model first asked for.
//!
//! Running the hooks twice — once for the rewrite and once for the refusal —
//! would be wrong twice over: a hook is a program with side effects, so a
//! deployment's audit log would double, and the two runs could disagree, which
//! would leave the call rewritten by one answer and judged by another. So the
//! hooks run **once**, at `tools/pre-execute`, and the answer they gave is held
//! for the `tools/permission` listener that immediately follows.
//!
//! [`PendingDecisions`] is that hold. It is keyed by call id, which is unique
//! within a step, and each entry is taken exactly once by the listener that
//! consumes it. A parallel group is safe because its calls have distinct ids.
//! An entry that is never taken is a call that never reached the gate, which
//! happens when an earlier call in the group faulted; those are swept when the
//! turn ends rather than left to grow.
//!
//! # What a missing answer means
//!
//! Absent is not permissive. If the gate finds no held decision it leaves the
//! declared permission exactly as it was, rather than treating the absence as
//! an approval — a bridge that failed to run must not read as a bridge that
//! allowed. The only thing that can lower a permission here is nothing at all:
//! [`crate::types::MergedDecision::Allow`] leaves the declared answer alone,
//! because a hook saying "I permit this" is not a hook saying "and nobody else
//! may object".
//!
//! Parity: upstream `packages/hooks/hooks-claude-code/src/index.ts` and
//! `packages/hooks/hooks-codex/src/index.ts`, the registration half.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tetanus_session::SessionLog;
use tetanus_turn::events::{
    PostToolDecision, PreStepDecision, ToolsPermission, ToolsPostExecute, ToolsPreExecute,
    TurnStopVeto,
};
use tetanus_turn::llm::Message;
use tetanus_turn::tools::{Permission, ToolCall};

use crate::events::{
    append_hook_invoked, append_hook_result, HookDialect, HookInvocation, HookResultRecord,
};
use crate::matcher::{matches_matcher, MatcherMode};
use crate::merge::merge_hook_outputs;
use crate::payload::{PayloadContext, ToolCallFacts};
use crate::runner::{run_hook, HookExecutor, RunHookOptions, DEFAULT_HOOK_TIMEOUT_MS};
use crate::types::{HookOutput, MergedDecision, MergedHookOutcome};
use crate::MatcherGroup;

/// The reference cap for a recorded stderr summary, when a deployment names
/// none.
pub const DEFAULT_STDERR_SUMMARY_MAX_CHARS: usize = 2000;

/// What a bridge needs to run one dialect's hooks.
pub struct BridgeConfig {
    /// The dialect being spoken, which decides payload shape and whether
    /// stdin ends with a newline.
    pub dialect: HookDialect,
    /// The `PreToolUse` groups, in configuration order.
    pub pre_tool_use: Vec<MatcherGroup>,
    /// The `PostToolUse` groups, in configuration order.
    pub post_tool_use: Vec<MatcherGroup>,
    /// The facts every payload is built from.
    pub context: PayloadContext,
    /// The model name, which only Codex payloads carry.
    pub model: String,
    /// How much of a hook's stderr is kept on `hook/result`.
    pub stderr_summary_max_chars: usize,
    /// Environment entries every hook process is started with.
    ///
    /// Claude Code exports `CLAUDE_PROJECT_DIR` and unmodified hooks in the
    /// wild use it to find project-relative files, so a bridge that dropped it
    /// would run those hooks successfully and have them look in the wrong
    /// place - which is worse than not running them at all.
    /// [`crate::discovery::DiscoveredHooks::env`] supplies it.
    pub env: Vec<(String, String)>,
}

impl BridgeConfig {
    /// A config for one dialect with no hooks configured, to be filled in.
    pub fn new(dialect: HookDialect, context: PayloadContext) -> Self {
        Self {
            dialect,
            pre_tool_use: Vec::new(),
            post_tool_use: Vec::new(),
            context,
            model: String::new(),
            stderr_summary_max_chars: DEFAULT_STDERR_SUMMARY_MAX_CHARS,
            env: Vec::new(),
        }
    }

    /// This bridge's context with the live turn written into it.
    ///
    /// The stored context is built once, when the bridge is composed, and its
    /// `turn` is a placeholder from that moment. Codex's turn-scoped payloads
    /// carry `turn_id`, so serving the stored value would tell every hook that
    /// every event happened in the same turn - and a hook correlating a
    /// `PreToolUse` with the `Stop` that followed would pair them across
    /// unrelated turns.
    fn context_at(&self, turn: u64) -> PayloadContext {
        PayloadContext {
            turn,
            ..self.context.clone()
        }
    }

    /// Whether a hook's stdin ends with a newline. Claude Code sends one,
    /// Codex does not, and this is the whole of that difference.
    fn trailing_newline(&self) -> bool {
        matches!(self.dialect, HookDialect::ClaudeCode)
    }

    /// How this dialect reads a matcher pattern. The two disagree, and
    /// `crates/hooks/src/matcher.rs` says where.
    fn matcher_mode(&self) -> MatcherMode {
        match self.dialect {
            HookDialect::ClaudeCode => MatcherMode::ClaudeCode,
            HookDialect::Codex => MatcherMode::Codex,
        }
    }
}

/// One `PreToolUse` answer, held between the rewrite point and the gate.
///
/// See the module note: the hooks run once, and this is where their answer
/// waits for the listener that can act on the forbidding half of it.
#[derive(Debug, Default)]
pub struct PendingDecisions {
    by_call: Mutex<HashMap<String, MergedHookOutcome>>,
}

impl PendingDecisions {
    /// Hold one call's answer, replacing any answer already held for it.
    ///
    /// Replacing rather than refusing: a retried call reuses its id, and the
    /// later run is the one that describes what is about to happen.
    pub fn put(&self, call_id: &str, outcome: MergedHookOutcome) {
        self.by_call
            .lock()
            .expect("pending")
            .insert(call_id.to_owned(), outcome);
    }

    /// Take one call's answer. A second take finds nothing, which is what
    /// stops one hook run deciding two calls.
    pub fn take(&self, call_id: &str) -> Option<MergedHookOutcome> {
        self.by_call.lock().expect("pending").remove(call_id)
    }

    /// Drop every held answer. The turn is over, so a call that never reached
    /// the gate never will.
    pub fn clear(&self) {
        self.by_call.lock().expect("pending").clear();
    }

    /// How many answers are held, for a caller checking nothing has leaked.
    pub fn len(&self) -> usize {
        self.by_call.lock().expect("pending").len()
    }

    /// Whether nothing is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The hooks one point selects for one tool name, with the pattern that chose
/// each - the pattern is part of the audit, so it travels with the hook.
fn selected<'a>(
    groups: &'a [MatcherGroup],
    tool_name: &str,
    mode: MatcherMode,
) -> Vec<(&'a MatcherGroup, &'a crate::runner::CommandHook)> {
    groups
        .iter()
        .filter(|group| matches_matcher(group.matcher.as_deref(), tool_name, mode))
        .flat_map(|group| group.hooks.iter().map(move |hook| (group, hook)))
        .collect()
}

/// Run every selected hook at one point and fold their answers into one.
///
/// The audit pair is written per hook and around the run, so a hook that
/// never returns still leaves `hook/invoked` on the journal - which is the
/// only way a reader can tell a hook that hung from a hook that was never
/// selected.
#[allow(clippy::too_many_arguments)]
async fn run_point(
    config: &BridgeConfig,
    executor: &dyn HookExecutor,
    log: &dyn SessionLog,
    clock: &(dyn Fn() -> u64 + Send + Sync),
    groups: &[MatcherGroup],
    point: &str,
    tool_name: &str,
    payload: Value,
    turn: u64,
) -> (MergedHookOutcome, Vec<HookOutput>) {
    let chosen = selected(groups, tool_name, config.matcher_mode());
    if chosen.is_empty() {
        return (MergedHookOutcome::default(), Vec::new());
    }
    let mut outputs = Vec::with_capacity(chosen.len());
    for (index, (group, hook)) in chosen.iter().enumerate() {
        let handler_id = format!("{point}-{turn}-{index}");
        let invocation = HookInvocation {
            turn,
            point: point.to_owned(),
            dialect: config.dialect,
            handler_id: handler_id.clone(),
            matcher: group.matcher.clone(),
        };
        // A journal that refuses the audit must not take the turn down: a
        // hook is a deployment's configuration, and the loudest correct
        // answer here is to run it anyway and let the result speak.
        let _ = append_hook_invoked(log, &invocation);

        let result = run_hook(
            executor,
            hook,
            RunHookOptions {
                payload: payload.clone(),
                env: (!config.env.is_empty()).then(|| config.env.clone()),
                cwd: Some(config.context.cwd.clone()),
                trailing_newline: config.trailing_newline(),
                default_timeout_ms: DEFAULT_HOOK_TIMEOUT_MS,
                expected_event: Some(point),
            },
            clock,
        )
        .await;

        let _ = append_hook_result(
            log,
            &HookResultRecord {
                turn,
                point: point.to_owned(),
                handler_id,
                output: result.output.clone(),
                stderr_summary_max_chars: config.stderr_summary_max_chars,
                duration_ms: result.duration_ms,
            },
        );
        outputs.push(result.output);
    }
    let merged = merge_hook_outputs(&outputs);
    (merged, outputs)
}

/// The facts a tool payload is built from, taken off the call the pipeline
/// holds.
fn facts(call: &ToolCall) -> ToolCallFacts {
    ToolCallFacts {
        tool_name: call.name.clone(),
        arguments: call.arguments.clone(),
        tool_use_id: call.id.clone(),
    }
}

/// Apply a `PreToolUse` rewrite to the call that is about to run.
///
/// Taken from the individual answers rather than the merged one, because
/// [`merge_hook_outputs`] deliberately has no opinion here: merging is
/// most-restrictive-wins, and there is no such thing as a more restrictive
/// rewrite. Two hooks rewriting one call is a configuration mistake either
/// way, so the bridge has to state a rule rather than inherit one, and the
/// rule is that the last hook to supply a rewrite wins - hooks run in
/// configuration order, and a later entry overriding an earlier one is what
/// every other layered configuration in this workspace does.
///
/// Only the rewrite half: the forbidding half is [`permission_from`], applied
/// at the gate.
pub fn apply_updated_input(call: &mut ToolCall, outputs: &[HookOutput]) {
    if let Some(updated) = outputs
        .iter()
        .rev()
        .find_map(|output| output.updated_input.as_ref())
    {
        call.arguments = Value::Object(updated.clone());
    }
}

/// Turn a merged `PreToolUse` answer into the permission it implies.
///
/// `None` means the hooks said nothing about permission, and the declared
/// answer stands unchanged. An `Allow` also leaves it unchanged: a hook
/// saying "I permit this" is not a hook saying "and nobody else may object",
/// and letting it lower the declared answer would let a hook un-gate a call a
/// tool author deliberately gated.
pub fn permission_from(outcome: &MergedHookOutcome) -> Option<Permission> {
    let reason = outcome
        .reason
        .clone()
        .unwrap_or_else(|| "a hook forbade this call".to_owned());
    match outcome.decision {
        MergedDecision::None | MergedDecision::Allow => None,
        MergedDecision::Ask => Some(Permission::ask(reason)),
        MergedDecision::Deny => Some(Permission::deny(reason)),
    }
}

/// Turn a merged `PostToolUse` answer into the contexts it contributes.
///
/// Each hook's text becomes its own message rather than one joined blob: they
/// came from different programs, and joining them would invent a single voice
/// for several unrelated notes.
pub fn contexts_from(outcome: &MergedHookOutcome) -> Vec<Message> {
    outcome
        .additional_context
        .iter()
        .filter(|text| !text.trim().is_empty())
        .map(Message::user)
        .collect()
}

/// Everything the two tool listeners share.
pub struct ToolHooks {
    pub config: BridgeConfig,
    pub executor: Arc<dyn HookExecutor>,
    pub log: Arc<dyn SessionLog>,
    pub pending: Arc<PendingDecisions>,
    /// The clock the audit's durations are measured on, injected so a case can
    /// assert a duration rather than tolerate one.
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl ToolHooks {
    /// Run `PreToolUse` for one call: rewrite it, and hold the answer for the
    /// gate that follows.
    pub async fn pre_tool_use(&self, turn: u64, call: &mut ToolCall) {
        let call_facts = facts(call);
        let payload = match self.config.dialect {
            HookDialect::ClaudeCode => {
                crate::payload::claude_pre_tool(&self.config.context, &call_facts)
            }
            HookDialect::Codex => crate::payload::codex_pre_tool(
                &self.config.context,
                &self.config.model,
                &call_facts,
            ),
        };
        let outcome = run_point(
            &self.config,
            self.executor.as_ref(),
            self.log.as_ref(),
            self.clock.as_ref(),
            &self.config.pre_tool_use,
            "PreToolUse",
            &call.name,
            payload,
            turn,
        )
        .await;
        let (outcome, outputs) = outcome;
        apply_updated_input(call, &outputs);
        self.pending.put(&call.id, outcome);
    }

    /// The permission the held `PreToolUse` answer implies for one call.
    pub fn gate(&self, call_id: &str, declared: Permission) -> Permission {
        // Absent is not permissive: a bridge that did not run leaves the
        // declared answer exactly as it was.
        let Some(outcome) = self.pending.take(call_id) else {
            return declared;
        };
        match permission_from(&outcome) {
            Some(from_hook) => declared.most_restrictive(from_hook),
            None => declared,
        }
    }

    /// Run `PostToolUse` for one settled call and return the contexts it
    /// contributed.
    pub async fn post_tool_use(&self, turn: u64, call: &ToolCall, response: &str) -> Vec<Message> {
        let call_facts = facts(call);
        let context = self.config.context_at(turn);
        let payload = match self.config.dialect {
            HookDialect::ClaudeCode => {
                crate::payload::claude_post_tool(&context, &call_facts, response)
            }
            HookDialect::Codex => {
                crate::payload::codex_post_tool(&context, &self.config.model, &call_facts, response)
            }
        };
        let outcome = run_point(
            &self.config,
            self.executor.as_ref(),
            self.log.as_ref(),
            self.clock.as_ref(),
            &self.config.post_tool_use,
            "PostToolUse",
            &call.name,
            payload,
            turn,
        )
        .await;
        contexts_from(&outcome.0)
    }
}

/// Register the two tool points on a bus, returning the handles that keep them
/// installed.
///
/// Dropping the handles takes the bridge back out, which is what makes a
/// deployment able to reload its hook configuration without restarting.
pub fn install_tool_hooks(
    bus: &tetanus_core::EventBus,
    hooks: Arc<ToolHooks>,
) -> Vec<tetanus_core::EffectHandle> {
    let rewrite = {
        let hooks = Arc::clone(&hooks);
        bus.on_waterfall::<ToolsPreExecute, _>(move |ev, next| {
            let hooks = Arc::clone(&hooks);
            Box::pin(async move {
                let turn = ev.turn;
                hooks.pre_tool_use(turn, &mut ev.call).await;
                next.run(ev).await
            })
        })
    };
    let gate = {
        let hooks = Arc::clone(&hooks);
        bus.on_waterfall::<ToolsPermission, _>(move |ev, next| {
            let hooks = Arc::clone(&hooks);
            let call_id = ev.call.id.clone();
            Box::pin(async move {
                let downstream = next.run(ev).await;
                hooks.gate(&call_id, downstream)
            })
        })
    };
    let after = {
        let hooks = Arc::clone(&hooks);
        bus.on_waterfall::<ToolsPostExecute, _>(move |ev, next| {
            let hooks = Arc::clone(&hooks);
            let turn = ev.turn;
            let call = ev.call.clone();
            Box::pin(async move {
                let downstream: PostToolDecision = next.run(ev).await;
                let content = downstream.outcome.content.clone();
                let mut decision = downstream;
                decision
                    .additional_contexts
                    .extend(hooks.post_tool_use(turn, &call, &content).await);
                decision
            })
        })
    };
    vec![rewrite, gate, after]
}

// ----------------------------------------------------- the observation points

/// The three points that watch rather than gate.
///
/// `SessionStart`, `UserPromptSubmit` and `Stop` carry no permission answer in
/// either dialect, so none of them can refuse a tool call. Two of them can
/// still change what happens, and the difference is worth stating because it
/// decides where each one registers.
///
/// `UserPromptSubmit` may add context to the prompt, and may refuse the prompt
/// outright. Both land at `agent/pre-step`, whose decision is exactly those
/// two options - enter with these messages, or reject.
///
/// `Stop` may ask that the turn *not* end. That is a veto on stopping, which
/// `agent/turn-stopping` already takes, so a hook wanting more work maps onto
/// the same seam a plugin uses for it. It is the one place where a hook's
/// `continue: false` means "keep going" rather than "halt": at every other
/// point the turn is running and halting is the exceptional request, whereas
/// here the turn is ending and continuing is.
///
/// `SessionStart` can only observe. Its context has nowhere to go that is not
/// the turn's own queue, and the queue belongs to the engine; contributing to
/// it from here would need a seam this crate does not have. That is recorded
/// as a gap rather than approximated, because a hook whose `additionalContext`
/// was silently dropped is worse than one that was never run.
pub struct WatchHooks {
    pub config: BridgeConfig,
    pub executor: Arc<dyn HookExecutor>,
    pub log: Arc<dyn SessionLog>,
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// The `SessionStart` groups.
    pub session_start: Vec<MatcherGroup>,
    /// The `UserPromptSubmit` groups.
    pub user_prompt_submit: Vec<MatcherGroup>,
    /// The `Stop` groups.
    pub stop: Vec<MatcherGroup>,
}

/// What a watch point answered.
pub struct Watched {
    /// Text the hooks contributed, one message per hook.
    pub contexts: Vec<Message>,
    /// The merged answer, for a caller that needs more than the contexts.
    pub outcome: MergedHookOutcome,
}

impl WatchHooks {
    /// Run one point whose selection is not about a tool.
    ///
    /// The matcher is tested against the *event name* rather than a tool name,
    /// because there is no tool here and a group configured with a pattern has
    /// to be given something to match; upstream does the same, which is why a
    /// match-all group is the ordinary configuration at these points.
    async fn watch(
        &self,
        groups: &[MatcherGroup],
        point: &str,
        payload: Value,
        turn: u64,
    ) -> Watched {
        let (outcome, _outputs) = run_point(
            &self.config,
            self.executor.as_ref(),
            self.log.as_ref(),
            self.clock.as_ref(),
            groups,
            point,
            point,
            payload,
            turn,
        )
        .await;
        Watched {
            contexts: contexts_from(&outcome),
            outcome,
        }
    }

    /// `SessionStart`: a session opened.
    ///
    /// `source` is Claude Code's word for why the session exists - `startup`,
    /// `resume`, `clear`. Codex's payload carries no such field, which is why
    /// it is a parameter here rather than part of the shared context.
    pub async fn session_start(&self, source: &str) -> Watched {
        let context = self.config.context_at(0);
        let payload = match self.config.dialect {
            HookDialect::ClaudeCode => crate::payload::claude_session_start(&context, source),
            HookDialect::Codex => crate::payload::codex_session_start(&context, &self.config.model),
        };
        let groups = self.session_start.clone();
        self.watch(&groups, "SessionStart", payload, 0).await
    }

    /// `UserPromptSubmit`: the person said something, before the model sees it.
    pub async fn user_prompt_submit(&self, turn: u64, prompt: &str) -> Watched {
        let context = self.config.context_at(turn);
        let payload = match self.config.dialect {
            HookDialect::ClaudeCode => crate::payload::claude_prompt(&context, prompt),
            HookDialect::Codex => {
                crate::payload::codex_prompt(&context, &self.config.model, prompt)
            }
        };
        let groups = self.user_prompt_submit.clone();
        self.watch(&groups, "UserPromptSubmit", payload, turn).await
    }

    /// `Stop`: the turn is about to end.
    pub async fn stop(&self, turn: u64) -> Watched {
        let context = self.config.context_at(turn);
        let payload = match self.config.dialect {
            HookDialect::ClaudeCode => crate::payload::claude_stop(&context),
            HookDialect::Codex => crate::payload::codex_stop(&context, &self.config.model),
        };
        let groups = self.stop.clone();
        self.watch(&groups, "Stop", payload, turn).await
    }
}

/// Whether a `UserPromptSubmit` answer refuses the prompt, and why.
///
/// A hook blocks a prompt by denying it or by asking the turn not to proceed;
/// both mean the model never sees what was typed. The words are the hook's,
/// because a person told only "blocked" cannot tell a policy from a fault.
pub fn prompt_refusal(outcome: &MergedHookOutcome) -> Option<String> {
    if outcome.decision == MergedDecision::Deny {
        return Some(
            outcome
                .reason
                .clone()
                .unwrap_or_else(|| "a hook refused this prompt".to_owned()),
        );
    }
    if outcome.stop {
        return Some(
            outcome
                .stop_reason
                .clone()
                .unwrap_or_else(|| "a hook stopped this prompt".to_owned()),
        );
    }
    None
}

/// Whether a `Stop` answer asks the turn to keep going, and why.
///
/// The inverted point. Everywhere else `continue: false` asks the turn to
/// halt; here the turn is already ending, so the same field is the only way a
/// hook can ask for more work - upstream's `decision: block` at `Stop` means
/// "do not stop yet".
pub fn stop_veto(outcome: &MergedHookOutcome) -> Option<String> {
    let blocked = outcome.decision == MergedDecision::Deny;
    if !blocked && !outcome.stop {
        return None;
    }
    Some(
        outcome
            .reason
            .clone()
            .or_else(|| outcome.stop_reason.clone())
            .unwrap_or_else(|| "a hook asked the turn to continue".to_owned()),
    )
}

/// Register `UserPromptSubmit` and `Stop` on a bus.
///
/// `SessionStart` is not here: it fires when a session opens rather than
/// inside a turn, so it is the composition's to call once, and there is no
/// turn event that means it.
pub fn install_watch_hooks(
    bus: &tetanus_core::EventBus,
    hooks: Arc<WatchHooks>,
) -> Vec<tetanus_core::EffectHandle> {
    let prompt = {
        let hooks = Arc::clone(&hooks);
        bus.on_waterfall::<tetanus_turn::events::PreStep, _>(move |ev, next| {
            let hooks = Arc::clone(&hooks);
            // Only the claim that carries what a person typed. A later step's
            // claim is the loop's own bookkeeping, and running a prompt hook
            // over it would tell the hook a user said something they did not.
            let typed: Option<String> = (ev.step == 1)
                .then(|| {
                    ev.messages
                        .iter()
                        .map(|m| m.content.clone())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .filter(|text| !text.is_empty());
            let turn = ev.turn;
            Box::pin(async move {
                let Some(prompt) = typed else {
                    return next.run(ev).await;
                };
                let watched = hooks.user_prompt_submit(turn, &prompt).await;
                if let Some(reason) = prompt_refusal(&watched.outcome) {
                    return PreStepDecision::Reject(reason);
                }
                match next.run(ev).await {
                    PreStepDecision::Enter(mut messages) => {
                        messages.extend(watched.contexts);
                        PreStepDecision::Enter(messages)
                    }
                    reject => reject,
                }
            })
        })
    };
    let ending = {
        let hooks = Arc::clone(&hooks);
        bus.on_serial::<tetanus_turn::events::TurnStopping, _>(move |ev| {
            let hooks = Arc::clone(&hooks);
            let turn = ev.turn;
            Box::pin(async move {
                let watched = hooks.stop(turn).await;
                stop_veto(&watched.outcome).map(|reason| TurnStopVeto { reason })
            })
        })
    };
    vec![prompt, ending]
}
