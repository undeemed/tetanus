//! Plan mode: the state in which the model works out what it would do before
//! it does any of it.
//!
//! **It is a fold over the log, not a flag on a process.** The last `plan/mode`
//! record wins and a journal with none is inactive, so a resumed or forked
//! session is in the mode its journal says it is in. A live mirror would be a
//! second copy of the state that a replay could disagree with.
//!
//! **The mode changes the prompt, never the tool catalogue.** While plan mode
//! is on, a guidance section joins the assembly; the exit tool stays registered
//! either way. That is upstream's choice and it is worth keeping for a reason
//! about models rather than about design: a tool that appears and disappears
//! between steps makes the request schema unstable, and a model that saw
//! `exit_plan_mode` in one step and not the next has to work out which world it
//! is in. Restricting what the model may *do* is the permission layer's job -
//! sandbox mode and the approval policy - and those read no plan state.
//!
//! **Leaving is an event, and the plan is its payload.** `exit_plan_mode`
//! carries the plan the model wrote and turns the mode off in one record, so
//! the journal shows the plan that was presented at the moment the session
//! stopped planning. A plan kept only in the conversation would be a plan a
//! transcript reader has to reconstruct from prose.
//!
//! Parity: upstream `packages/plan/plan-mode`, pinned by its
//! `integration.spec.ts` and `invariant.spec.ts`. Upstream defers a user's
//! selection until the next accepted pre-step, so a flip lands on a step
//! boundary rather than inside one; tetanus records the flip when it is made
//! and `docs/parity.md` says why the difference is not observable at the
//! seam this crate owns.

use std::sync::Arc;

use serde_json::{json, Value};
use tetanus_session::{SessionError, SessionEvent, SessionLog};
use tetanus_turn::prompt::{PromptError, PromptRegistry, Section, SectionText};
use tetanus_turn::tools::{Tool, ToolError, ToolOutcome, ToolSchema};

/// The durable type this module writes.
pub mod topic {
    /// Whether plan mode is in force from this point on. Whole-value replace,
    /// last one wins, and never model-visible: what the model knows about the
    /// mode is the guidance section it reads.
    pub const PLAN_MODE: &str = "plan/mode";
    /// The plan the model presented when it left plan mode.
    pub const PLAN_PRESENTED: &str = "plan/presented";
}

/// The section id the guidance is registered under.
pub const SECTION: &str = "plan:policy";

/// Whether plan mode is in force at the end of this journal.
///
/// A log with no record folds to inactive: planning is the mode a deployment
/// opts into, not the one it has to opt out of.
pub fn active(events: &[SessionEvent]) -> bool {
    events
        .iter()
        .rev()
        .find(|event| event.ty == topic::PLAN_MODE)
        .and_then(|event| event.data["active"].as_bool())
        .unwrap_or(false)
}

/// The last plan the model presented, if it has presented one.
pub fn presented(events: &[SessionEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|event| event.ty == topic::PLAN_PRESENTED)
        .and_then(|event| event.data["plan"].as_str())
        .map(str::to_string)
}

/// Turn plan mode on or off.
///
/// Writing the state it is already in appends nothing, so a caller may send it
/// idempotently - the same rule the approval policy follows, and for the same
/// reason: a journal full of records that changed nothing makes the ones that
/// did hard to find.
///
/// Answers whether the journal was written.
pub fn set(log: &dyn SessionLog, on: bool) -> Result<bool, SessionError> {
    if active(&log.events()) == on {
        return Ok(false);
    }
    log.append(topic::PLAN_MODE, json!({ "active": on }))?;
    Ok(true)
}

/// Register the guidance section that is rendered while plan mode is on.
///
/// The section is registered once and answers empty while the mode is off,
/// rather than being added and removed as the mode flips. A section that comes
/// and goes is a prompt whose shape changes under the model for a reason it
/// cannot see; one that renders empty contributes nothing at all, because an
/// empty section is dropped by the assembly.
///
/// The handle is the registration: dropping it takes the section back out.
pub fn guidance(
    prompt: &Arc<PromptRegistry>,
    log: Arc<dyn SessionLog>,
    text: impl Into<String>,
    order: i32,
) -> Result<tetanus_core::EffectHandle, PromptError> {
    let text = text.into();
    prompt.section(Section::new(
        SECTION,
        order,
        SectionText::provided(move |_at| {
            if active(&log.events()) {
                text.clone()
            } else {
                String::new()
            }
        }),
    ))
}

/// The tool that ends plan mode by presenting the plan.
pub struct ExitPlanModeTool {
    log: Arc<dyn SessionLog>,
}

impl ExitPlanModeTool {
    pub const NAME: &'static str = "exit_plan_mode";

    pub fn new(log: Arc<dyn SessionLog>) -> Arc<Self> {
        Arc::new(Self { log })
    }
}

#[async_trait::async_trait]
impl Tool for ExitPlanModeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Present the plan you have worked out and leave plan mode. Call it only \
                          when the plan is complete: it is how the work is put to the user for \
                          review."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "The complete plan, in markdown, for the user to review.",
                    },
                },
                "required": ["plan"],
            }),
        }
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let plan = arguments
            .get("plan")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if plan.is_empty() {
            return Ok(ToolOutcome::failed(
                "`plan` must say what you intend to do: leaving plan mode with no plan gives the \
                 user nothing to review.",
            ));
        }
        // The tool stays callable while the mode is off, because the catalogue
        // must not change with the mode. Calling it then is answered rather
        // than refused: nothing is wrong, there is simply no mode to leave.
        if !active(&self.log.events()) {
            return Ok(ToolOutcome::ok(
                "Plan mode is not on, so there is nothing to leave. Carry on with the work.",
            ));
        }

        self.log
            .append(topic::PLAN_PRESENTED, json!({ "plan": plan }))
            .map_err(|e| ToolError::Failed(Self::NAME.into(), e.to_string()))?;
        set(self.log.as_ref(), false)
            .map_err(|e| ToolError::Failed(Self::NAME.into(), e.to_string()))?;

        Ok(ToolOutcome::ok(
            "Plan presented and plan mode left. Proceed with the plan.",
        ))
    }
}
