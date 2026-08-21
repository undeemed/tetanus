//! The task list the model keeps while it works.
//!
//! **The whole list every call.** There are no partial updates and no per-item
//! edits: a call carries the complete list and replaces the previous one. That
//! is upstream's shape and it is the right one for a model-facing tool - a
//! per-item protocol needs stable item ids, which means the model has to
//! remember them across steps, and a model that misremembers one silently edits
//! the wrong task. Sending everything makes the call idempotent and the journal
//! record self-contained.
//!
//! **The journal is the list.** Each call appends one `todo/write` snapshot,
//! and the list is the last one of those - a fold, not a field. A resumed
//! session therefore sees the list it had with nothing to restore, and a
//! transcript shows how the list changed over the run rather than only where it
//! ended.
//!
//! **The standing list is cleared by the next turn, not by the end of this
//! one.** `turn/end` leaves the finished checklist visible, because a person
//! reading the end of a turn wants to see what was done; the next `turn/start`
//! clears it, because a list from the previous turn presented as current is a
//! plan nobody made. Upstream's projection makes the same choice.
//!
//! **One active task, unless the deployment says otherwise.** A model marking
//! six things in progress is reporting nothing. The single-active discipline is
//! the default and the description says so; a deployment that runs work
//! genuinely in parallel switches it, and the description changes with it - the
//! rule the tool enforces and the rule the model is told must be the same rule,
//! or the model is being punished for following its instructions.
//!
//! Parity: upstream `packages/todo/tool-todo`, pinned by its `tool-todo.spec.ts`,
//! `integration.spec.ts` and `projection.spec.ts`.

use std::sync::Arc;

use serde_json::{json, Value};
use tetanus_session::{SessionError, SessionEvent, SessionLog};
use tetanus_turn::log::topic as turn_topic;
use tetanus_turn::tools::{Tool, ToolError, ToolOutcome, ToolSchema};

/// The durable type this module writes.
pub mod topic {
    /// One whole-list snapshot. The last one on the journal is the list, and
    /// nothing else is.
    pub const TODO_WRITE: &str = "todo/write";
}

/// Where one task has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    InProgress,
    Completed,
}

impl Status {
    pub const ALL: [Status; 3] = [Status::Pending, Status::InProgress, Status::Completed];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }
}

/// One task on the list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    /// A short imperative line. Trimmed, and the trimmed form is the identity:
    /// it is what is stored, what is shown, and what duplicate detection
    /// compares.
    pub content: String,
    pub status: Status,
}

/// How many tasks are in each state.
///
/// Answered back to the model with every write, because a model that has just
/// sent eleven items should not have to count them to know what it said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Counts {
    pub pending: usize,
    pub in_progress: usize,
    pub completed: usize,
}

impl Counts {
    pub fn of(todos: &[TodoItem]) -> Self {
        let mut counts = Self::default();
        for todo in todos {
            match todo.status {
                Status::Pending => counts.pending += 1,
                Status::InProgress => counts.in_progress += 1,
                Status::Completed => counts.completed += 1,
            }
        }
        counts
    }
}

/// Why a list was refused.
///
/// Each is a mistake the model can correct from the message alone, which is the
/// bar for anything a tool says back: a refusal the model cannot act on costs a
/// step and teaches it nothing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TodoError {
    #[error("todo {index} has no content: every task needs a short imperative line")]
    Empty { index: usize },
    #[error(
        "the list names {content:?} twice: a task's content is its identity, so give the two \
         tasks different lines"
    )]
    Duplicate { content: String },
    #[error(
        "{count} tasks are in_progress at once, and this deployment expects at most one: mark \
         the one being worked on now, and leave the rest pending"
    )]
    TooManyActive { count: usize },
}

/// Whether several tasks may be `in_progress` at once.
///
/// A deployment choice with no default worth guessing, so the composer states
/// it. Upstream makes it a required config field for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parallelism {
    /// At most one task in progress. The discipline that makes the list mean
    /// something for sequential work.
    SingleActive,
    /// Several at once, for a deployment whose work genuinely runs in parallel.
    Parallel,
}

impl Parallelism {
    fn allows(self, active: usize) -> bool {
        match self {
            Self::SingleActive => active <= 1,
            Self::Parallel => {
                let _ = active;
                true
            }
        }
    }
}

/// Check and canonicalize a list the model sent.
///
/// Trimming happens before every other judgement, because the trimmed form is
/// what is stored and duplicate detection must compare what will exist rather
/// than what was typed.
pub fn canonical(raw: &[TodoItem], parallelism: Parallelism) -> Result<Vec<TodoItem>, TodoError> {
    let mut todos: Vec<TodoItem> = Vec::with_capacity(raw.len());
    let mut active = 0;
    for (index, item) in raw.iter().enumerate() {
        let content = item.content.trim().to_string();
        if content.is_empty() {
            return Err(TodoError::Empty { index });
        }
        if todos.iter().any(|kept| kept.content == content) {
            return Err(TodoError::Duplicate { content });
        }
        if item.status == Status::InProgress {
            active += 1;
        }
        todos.push(TodoItem {
            content,
            status: item.status,
        });
    }
    if !parallelism.allows(active) {
        return Err(TodoError::TooManyActive { count: active });
    }
    Ok(todos)
}

/// The list a journal currently holds, or `None` when there is none to show.
///
/// The fold is the whole state. `None` before the first write, and `None` again
/// once a later turn has started: a list written in an earlier turn is history,
/// and presenting it as the current plan would be showing a plan nobody made.
/// A `turn/end` does not clear it, so the checklist a turn finished with stays
/// readable at the end of that turn.
pub fn current(events: &[SessionEvent]) -> Option<Vec<TodoItem>> {
    let mut list: Option<Vec<TodoItem>> = None;
    for event in events {
        match event.ty.as_str() {
            topic::TODO_WRITE => {
                // A snapshot this build cannot read is skipped rather than
                // fatal: the journal outlives the build that wrote it, and a
                // list that fails to parse must not make the session
                // unreadable. It leaves the previous list standing, which is
                // the conservative direction - the alternative is claiming
                // there is no list when there is one.
                if let Ok(todos) =
                    serde_json::from_value::<Vec<TodoItem>>(event.data["todos"].clone())
                {
                    list = Some(todos);
                }
            }
            turn_topic::TURN_START => list = None,
            _ => {}
        }
    }
    list
}

/// Append one whole-list snapshot.
pub fn write(log: &dyn SessionLog, todos: &[TodoItem]) -> Result<SessionEvent, SessionError> {
    log.append(topic::TODO_WRITE, json!({ "todos": todos }))
}

/// The model-facing tool.
pub struct TodoWriteTool {
    log: Arc<dyn SessionLog>,
    parallelism: Parallelism,
}

impl TodoWriteTool {
    pub const NAME: &'static str = "todo_write";

    pub fn new(log: Arc<dyn SessionLog>, parallelism: Parallelism) -> Arc<Self> {
        Arc::new(Self { log, parallelism })
    }

    /// The sentence that varies with the policy.
    ///
    /// Split out because it is the only part the deployment choice changes, and
    /// because the enforcement above and this text must not drift: a model told
    /// to mark everything active and then refused for doing it has been set up
    /// to fail.
    fn active_rule(&self) -> &'static str {
        match self.parallelism {
            Parallelism::SingleActive => {
                "Keep AT MOST ONE task in_progress at a time; while work remains, exactly one \
                 should be in_progress. "
            }
            Parallelism::Parallel => {
                "Mark every task being actively worked on in_progress - several at once when work \
                 genuinely runs in parallel; while work remains, at least one should be \
                 in_progress. "
            }
        }
    }
}

#[async_trait::async_trait]
impl Tool for TodoWriteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: Self::NAME.into(),
            description: format!(
                "Record and update the task list for the current work. Send the ENTIRE list every \
                 call - it REPLACES the previous list, and there are no partial updates or \
                 per-item edits. Add one task per concrete step before you start. {}Mark a task \
                 completed the moment it is done rather than batching completions. Skip the list \
                 for trivial single-step work.",
                self.active_rule()
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The COMPLETE task list, replacing any previous list.",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "What the task is - a short imperative line.",
                                },
                                "status": {
                                    "type": "string",
                                    "enum": Status::ALL.map(Status::as_str).to_vec(),
                                    "description": "pending (not started) | in_progress (now) | \
                                                    completed (done).",
                                },
                            },
                            "required": ["content", "status"],
                        },
                    },
                },
                "required": ["todos"],
            }),
        }
    }

    async fn execute(&self, arguments: &Value) -> Result<ToolOutcome, ToolError> {
        let raw: Vec<TodoItem> =
            serde_json::from_value(arguments.get("todos").cloned().unwrap_or(json!(null)))
                .map_err(|e| {
                    ToolError::InvalidArguments(
                        Self::NAME.into(),
                        format!("`todos` must be a list of {{content, status}} items: {e}"),
                    )
                })?;

        // A list the model got wrong is told to the model, not raised as a tool
        // failure: it can send a corrected list on the next step, and a failure
        // would say the tool is broken when the arguments were.
        let todos = match canonical(&raw, self.parallelism) {
            Ok(todos) => todos,
            Err(refused) => return Ok(ToolOutcome::failed(refused.to_string())),
        };

        // Durable before the model is told. A model that read "recorded" for a
        // list the journal does not carry would plan against a list a replay
        // cannot show.
        write(self.log.as_ref(), &todos)
            .map_err(|e| ToolError::Failed(Self::NAME.into(), e.to_string()))?;

        let counts = Counts::of(&todos);
        Ok(ToolOutcome::ok(
            serde_json::to_string(&json!({ "todos": todos, "counts": counts }))
                .unwrap_or_else(|_| "{}".to_string()),
        ))
    }
}
