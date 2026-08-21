//! The standing objective a session works toward.
//!
//! A todo list says what the steps are; a goal says what finishing looks like.
//! It outlives a turn, which is why it is durable, and it is one per session,
//! which is why replacing it is a decision rather than a write.
//!
//! **Every change carries the whole state.** A `goal/changed` record is the
//! complete post-change goal, not a patch, so the fold is last-wins and a
//! reader that starts mid-journal still sees a coherent goal. Upstream makes
//! the same choice for the same reason.
//!
//! **A mutation names the revision it is changing.** The revision counts up
//! from one and every accepted change increments it. A caller that read the
//! goal three steps ago and now asks to complete it is asked to read again,
//! because the objective it believes it is completing may not be the one that
//! is there - and a model that pauses the wrong goal cannot tell that anything
//! went wrong. This is the same compare-and-set upstream applies, restated as a
//! fold over the journal rather than as a cache with a version field.
//!
//! **Replacement is only ever explicit.** A create over an unfinished goal is
//! refused: the alternative is a model quietly abandoning work a person asked
//! for by starting something else. Completing or clearing it first is one extra
//! call and makes the abandonment visible on the journal.
//!
//! **A cleared goal is a tombstone, not a gap.** Clearing appends a record
//! saying so, so a journal can distinguish "there was never a goal" from "the
//! goal was dropped" - a distinction a transcript needs and an absence cannot
//! carry.
//!
//! Parity: upstream `packages/goal/goal` and `packages/goal/tool-goal`, pinned
//! by their `goal.spec.ts`, `projection.spec.ts` and `tool-goal.spec.ts`. The
//! autonomous round driver is not restated; `docs/parity-updates/` says why.

use std::sync::Arc;

use serde_json::{json, Value};
use tetanus_session::{SessionError, SessionEvent, SessionLog};
use tetanus_turn::tools::{Tool, ToolError, ToolOutcome, ToolSchema};

/// The durable type this module writes.
pub mod topic {
    /// The complete post-change goal, or the tombstone a clear leaves.
    pub const GOAL_CHANGED: &str = "goal/changed";
}

/// Where a goal has got to.
///
/// Durable, and separate from anything process-local: a resumed session is in
/// the phase its journal says, whatever the process that resumed it is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Being worked on.
    Active,
    /// Deliberately set down, and resumable.
    Paused,
    /// Stopped on something outside the session's control, with a reason.
    Blocked,
    /// Finished. A terminal phase: the way out is a new goal.
    Complete,
}

impl Phase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::Complete => "complete",
        }
    }

    /// Whether a goal in this phase still stands in the way of a new one.
    ///
    /// A method rather than a comparison at each call site: "unfinished" is the
    /// rule that decides whether a create is a replacement, and a second
    /// spelling of it is a second thing to get wrong.
    pub const fn unfinished(self) -> bool {
        match self {
            Self::Active | Self::Paused | Self::Blocked => true,
            Self::Complete => false,
        }
    }
}

/// Why a goal is blocked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Blocker {
    /// A stable lower-kebab-case classification, so a surface or a policy can
    /// route on it without reading prose.
    pub code: String,
    /// What a person needs to know. Non-empty: a blocked goal with no
    /// explanation is a session that has stopped for no stated reason.
    pub message: String,
}

/// The whole goal, as one `goal/changed` record carries it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Goal {
    /// Counts up from one; every accepted change increments it. It is the
    /// compare-and-set token, and it is also how a reader knows two records
    /// describe the same goal at different times.
    pub revision: u64,
    pub objective: String,
    pub phase: Phase,
    /// Present exactly while the phase is `blocked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<Blocker>,
}

/// What one caller asked to do to the goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Start one. Refused while an unfinished goal stands.
    Create {
        objective: String,
    },
    /// Change the objective of the goal at `revision`.
    Edit {
        revision: u64,
        objective: String,
    },
    Pause {
        revision: u64,
    },
    Resume {
        revision: u64,
    },
    Block {
        revision: u64,
        blocker: Blocker,
    },
    Complete {
        revision: u64,
    },
    /// Drop it, leaving a tombstone.
    Clear {
        revision: u64,
    },
}

impl Operation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Edit { .. } => "edit",
            Self::Pause { .. } => "pause",
            Self::Resume { .. } => "resume",
            Self::Block { .. } => "block",
            Self::Complete { .. } => "complete",
            Self::Clear { .. } => "clear",
        }
    }

    /// The revision this operation claims to be changing, if it changes one.
    fn revision(&self) -> Option<u64> {
        match self {
            Self::Create { .. } => None,
            Self::Edit { revision, .. }
            | Self::Pause { revision }
            | Self::Resume { revision }
            | Self::Block { revision, .. }
            | Self::Complete { revision }
            | Self::Clear { revision } => Some(*revision),
        }
    }
}

/// Why a change was refused.
///
/// Every message says what the caller should do next, because the caller is
/// usually a model and its next move is decided by what it reads.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GoalError {
    #[error("a goal needs an objective: say what finishing looks like")]
    NoObjective,
    #[error(
        "this session already has an unfinished goal ({objective:?}, {phase}). Complete or clear \
         it before starting another, so abandoning it is on the record"
    )]
    AlreadyGoing {
        objective: String,
        phase: &'static str,
    },
    #[error("there is no goal to {operation}: create one first")]
    NoGoal { operation: &'static str },
    #[error(
        "the goal is at revision {current}, not {claimed}: read it again and reapply the change \
         to what is there now"
    )]
    Stale { claimed: u64, current: u64 },
    #[error("a goal cannot go from {from} to {to}")]
    BadTransition {
        from: &'static str,
        to: &'static str,
    },
    #[error("a blocked goal needs a reason: give a short code and a sentence a person can read")]
    NoBlocker,
}

/// The goal a journal currently holds, or `None` before the first create and
/// after a clear.
///
/// The fold is the whole state: every record carries the complete goal, so the
/// last one wins and a tombstone answers `None`.
pub fn current(events: &[SessionEvent]) -> Option<Goal> {
    let mut goal = None;
    for event in events {
        if event.ty != topic::GOAL_CHANGED {
            continue;
        }
        if event.data["operation"] == json!("clear") {
            goal = None;
            continue;
        }
        // A record this build cannot read leaves the previous goal standing,
        // for the reason `todo::current` gives: a journal outlives its writer,
        // and claiming there is no goal when there is one is the worse answer.
        if let Ok(read) = serde_json::from_value::<Goal>(event.data["goal"].clone()) {
            goal = Some(read);
        }
    }
    goal
}

/// Whether this journal ever held a goal that was then cleared.
///
/// Published because absence and abandonment are different facts: a surface
/// showing "no goal" for a session whose goal was dropped is hiding a decision
/// somebody made.
pub fn was_cleared(events: &[SessionEvent]) -> bool {
    events
        .iter()
        .rev()
        .find(|event| event.ty == topic::GOAL_CHANGED)
        .is_some_and(|event| event.data["operation"] == json!("clear"))
}

/// Apply one operation to what the journal holds, and commit the result.
///
/// The whole rule set lives in [`decide`], so a caller reaching the domain
/// directly - a command, a test, a later surface - gets the same answers as a
/// model does. This adds the two things a tool needs on top: the journal, and
/// telling a journal failure apart from a rule the caller broke, because only
/// the second is the caller's to fix.
pub fn commit(log: &dyn SessionLog, operation: Operation) -> Result<Goal, CommitError> {
    let events = log.events();
    let existing = current(&events);
    let next = decide(existing, &operation)?;
    let data = match &operation {
        Operation::Clear { .. } => json!({
            "operation": "clear",
            "cleared": { "revision": next.revision, "objective": next.objective },
        }),
        _ => json!({ "operation": operation.as_str(), "goal": next }),
    };
    log.append(topic::GOAL_CHANGED, data)?;
    Ok(next)
}

#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    #[error(transparent)]
    Refused(#[from] GoalError),
    #[error(transparent)]
    Log(#[from] SessionError),
}

/// What the goal becomes, without writing anything.
///
/// Separated from the commit so the rules can be read - and tested - without a
/// journal, and so a caller that wants to know whether a move is legal does not
/// have to make it.
pub fn decide(existing: Option<Goal>, operation: &Operation) -> Result<Goal, GoalError> {
    match (operation, existing) {
        (Operation::Create { objective }, None) => Ok(Goal {
            revision: 1,
            objective: checked(objective)?,
            phase: Phase::Active,
            blocker: None,
        }),
        (Operation::Create { objective }, Some(goal)) => {
            if goal.phase.unfinished() {
                return Err(GoalError::AlreadyGoing {
                    objective: goal.objective,
                    phase: goal.phase.as_str(),
                });
            }
            Ok(Goal {
                revision: goal.revision + 1,
                objective: checked(objective)?,
                phase: Phase::Active,
                blocker: None,
            })
        }
        (other, None) => Err(GoalError::NoGoal {
            operation: other.as_str(),
        }),
        (other, Some(goal)) => {
            if let Some(claimed) = other.revision() {
                if claimed != goal.revision {
                    return Err(GoalError::Stale {
                        claimed,
                        current: goal.revision,
                    });
                }
            }
            transition(goal, other)
        }
    }
}

/// The legal moves from one phase to the next.
///
/// Stated as a match on the pair rather than as a table of booleans, so the
/// compiler is the thing that notices a phase nobody handled.
fn transition(goal: Goal, operation: &Operation) -> Result<Goal, GoalError> {
    match operation {
        Operation::Create { .. } => unreachable!("create is settled before the transition table"),
        Operation::Edit { objective, .. } => edited(goal, objective),
        Operation::Pause { .. } => moved(goal, Phase::Paused, &[Phase::Active, Phase::Blocked]),
        Operation::Resume { .. } => moved(goal, Phase::Active, &[Phase::Paused, Phase::Blocked]),
        Operation::Block { blocker, .. } => blocked(goal, blocker),
        // Completion is allowed from every unfinished phase: a goal that turned
        // out to be done while it was paused or blocked is done, and forcing a
        // resume first would put a lie on the journal.
        Operation::Complete { .. } => moved(
            goal,
            Phase::Complete,
            &[Phase::Active, Phase::Paused, Phase::Blocked],
        ),
        // Clearing is always legal: it is how a session gets out of any state,
        // and the tombstone records that it happened.
        Operation::Clear { .. } => Ok(Goal {
            revision: goal.revision + 1,
            ..goal
        }),
    }
}

/// Move a goal to `to`, when it is resting in a phase that permits the move.
///
/// The permitted set is passed in rather than matched here, so each transition
/// reads as one line at the call site and adding a phase means revisiting one
/// list instead of re-reading five nested matches. A move always clears the
/// blocker: every phase this reaches is one where the old reason no longer
/// describes the goal.
fn moved(goal: Goal, to: Phase, from: &[Phase]) -> Result<Goal, GoalError> {
    if !from.contains(&goal.phase) {
        return Err(GoalError::BadTransition {
            from: goal.phase.as_str(),
            to: to.as_str(),
        });
    }
    Ok(Goal {
        revision: goal.revision + 1,
        phase: to,
        blocker: None,
        ..goal
    })
}

/// A finished goal is not edited back to life: that is a new goal, and saying
/// so keeps the journal's record of what was attempted honest.
fn edited(goal: Goal, objective: &str) -> Result<Goal, GoalError> {
    if goal.phase == Phase::Complete {
        return Err(GoalError::BadTransition {
            from: goal.phase.as_str(),
            to: "edited",
        });
    }
    Ok(Goal {
        revision: goal.revision + 1,
        objective: checked(objective)?,
        ..goal
    })
}

/// Block a goal, on the reason it is blocked by.
///
/// The reason is checked before the phase, because a blocked goal with no
/// explanation is worth refusing whatever phase it was resting in.
fn blocked(goal: Goal, blocker: &Blocker) -> Result<Goal, GoalError> {
    if blocker.code.trim().is_empty() || blocker.message.trim().is_empty() {
        return Err(GoalError::NoBlocker);
    }
    let reason = Blocker {
        code: blocker.code.trim().to_string(),
        message: blocker.message.trim().to_string(),
    };
    let moved = moved(goal, Phase::Blocked, &[Phase::Active, Phase::Paused])?;
    Ok(Goal {
        blocker: Some(reason),
        ..moved
    })
}

fn checked(objective: &str) -> Result<String, GoalError> {
    let objective = objective.trim();
    if objective.is_empty() {
        return Err(GoalError::NoObjective);
    }
    Ok(objective.to_string())
}

/// Read the current goal.
pub struct GoalReadTool {
    log: Arc<dyn SessionLog>,
}

/// Create or change it.
pub struct GoalWriteTool {
    log: Arc<dyn SessionLog>,
}

impl GoalReadTool {
    pub const NAME: &'static str = "get_goal";

    pub fn new(log: Arc<dyn SessionLog>) -> Arc<Self> {
        Arc::new(Self { log })
    }
}

impl GoalWriteTool {
    pub const NAME: &'static str = "update_goal";

    pub fn new(log: Arc<dyn SessionLog>) -> Arc<Self> {
        Arc::new(Self { log })
    }
}

#[async_trait::async_trait]
impl Tool for GoalReadTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Read this session's standing goal: its objective, its phase, and the \
                          revision any change to it must name."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    /// Reading changes nothing, so any number of reads may overlap.
    fn mode(&self, _arguments: &Value) -> tetanus_turn::tools::ToolMode {
        tetanus_turn::tools::ToolMode::Parallel
    }

    async fn execute(&self, _arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let events = self.log.events();
        Ok(ToolOutcome::ok(match current(&events) {
            Some(goal) => {
                serde_json::to_string(&json!({ "goal": goal })).unwrap_or_else(|_| "{}".to_string())
            }
            // The two absences are told apart, because they are different
            // facts and only one of them means somebody decided something.
            None if was_cleared(&events) => {
                json!({ "goal": Value::Null, "cleared": true }).to_string()
            }
            None => json!({ "goal": Value::Null, "cleared": false }).to_string(),
        }))
    }
}

#[async_trait::async_trait]
impl Tool for GoalWriteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: "Set or change this session's standing goal. Every action except create \
                          names the revision it is changing, which get_goal reports; a stale \
                          revision is refused so a change always lands on the goal you read."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "edit", "pause", "resume", "block", "complete", "clear"],
                        "description": "What to do to the goal.",
                    },
                    "objective": {
                        "type": "string",
                        "description": "What finishing looks like. Required by create and edit.",
                    },
                    "revision": {
                        "type": "integer",
                        "description": "The revision being changed, as get_goal reported it. \
                                        Required by every action except create.",
                    },
                    "blocker_code": {
                        "type": "string",
                        "description": "Short lower-kebab-case classification. Required by block.",
                    },
                    "blocker_message": {
                        "type": "string",
                        "description": "One sentence a person can act on. Required by block.",
                    },
                },
                "required": ["action"],
            }),
        }
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let operation = match read_operation(arguments) {
            Ok(operation) => operation,
            // A missing or wrong-shaped argument is the model's to fix and is
            // told to the model, not raised as a tool failure.
            Err(why) => return Ok(ToolOutcome::failed(why)),
        };

        match commit(self.log.as_ref(), operation) {
            Ok(goal) => Ok(ToolOutcome::ok(
                serde_json::to_string(&json!({ "goal": goal }))
                    .unwrap_or_else(|_| "{}".to_string()),
            )),
            Err(CommitError::Refused(refused)) => Ok(ToolOutcome::failed(refused.to_string())),
            Err(CommitError::Log(e)) => Err(ToolError::Failed(Self::NAME.into(), e.to_string())),
        }
    }
}

/// Read one call's arguments into an operation, or say which argument the
/// action needed.
///
/// The conditional arguments are why this is a function rather than a serde
/// derive: `revision` is required by six actions and refused by the seventh,
/// and a model that omitted one should be told which one it omitted.
fn read_operation(arguments: &Value) -> Result<Operation, String> {
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "`action` must be one of create, edit, pause, resume, block, complete, clear"
                .to_string()
        })?;
    let objective = || {
        arguments
            .get("objective")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("`{action}` needs an `objective`"))
    };
    let revision = || {
        arguments
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                format!("`{action}` needs the `revision` it is changing; get_goal reports it")
            })
    };

    Ok(match action {
        "create" => Operation::Create {
            objective: objective()?,
        },
        "edit" => Operation::Edit {
            revision: revision()?,
            objective: objective()?,
        },
        "pause" => Operation::Pause {
            revision: revision()?,
        },
        "resume" => Operation::Resume {
            revision: revision()?,
        },
        "complete" => Operation::Complete {
            revision: revision()?,
        },
        "clear" => Operation::Clear {
            revision: revision()?,
        },
        "block" => Operation::Block {
            revision: revision()?,
            blocker: Blocker {
                code: arguments
                    .get("blocker_code")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                message: arguments
                    .get("blocker_message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
        },
        other => {
            return Err(format!(
                "{other:?} is not an action: use create, edit, pause, resume, block, complete or \
                 clear"
            ))
        }
    })
}
