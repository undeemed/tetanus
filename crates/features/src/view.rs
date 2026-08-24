//! What a surface reads: the feature state of one session, as data.
//!
//! The web UI crew cannot build a workspace panel or an attachment strip
//! against `todo::current`, `goal::current` and the rest, because those answer
//! this crate's own types and a surface is on the other side of a boundary from
//! them. This module is the vocabulary between the two: serde types with stable
//! field names, folded from the journal, carrying no engine type and no trait
//! object.
//!
//! **Every view is a fold, and it says how far it folded.** [`SessionView::as_of_seq`]
//! is the journal position the view describes, so a panel that receives two
//! views out of order can tell which is newer, and a panel showing a stale one
//! can say so. A view with no sequence would make a live surface guess.
//!
//! **A view carries no bytes.** An attachment is named, measured and described;
//! its content is fetched by id from the object store. A base64 screenshot
//! inside a push frame is a frame nobody can read, a log line nobody can grep,
//! and a memory spike on every subscriber - and the surface that wants a
//! thumbnail wants it once, not on every fold.
//!
//! **A view carries no presentation.** No markdown rendering, no truncation, no
//! localized text, no colour. Those are the surface's decisions and it has more
//! context for them than this crate does - it knows the width. What this owes
//! the surface is the facts, spelled the same way every time.
//!
//! **Adding a field is a minor change; removing or renaming one is not.** The
//! rule `docs/interface-contract.md` §5 states for the boundary types applies
//! here for the same reason, and it costs a consumer one thing: match a struct
//! with a rest pattern, or a field added later stops your build.
//!
//! The shapes are published in `docs/interface-contract.md` §5.1,
//! which is what a surface author reads before writing a panel.

use std::path::{Path, PathBuf};

use tetanus_session::SessionEvent;

use crate::attachment::{self, Attachment};
use crate::feedback;
use crate::goal::{self, Goal};
use crate::plan;
use crate::todo::{self, Counts, TodoItem};
use crate::workspace::Workspace;

/// One session's feature state, as a surface reads it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionView {
    /// The last journal sequence this view folded. A surface compares it to
    /// decide whether a view it holds is still current.
    pub as_of_seq: i64,
    /// The task list, or `null` before the first write and after the next turn
    /// began. Absent and empty are different: `null` is "no plan yet", `[]` is
    /// "a plan the model emptied".
    pub todos: Option<TodoListView>,
    pub goal: Option<GoalView>,
    pub plan: PlanView,
    pub feedback: FeedbackView,
    /// Every attachment the session admitted, oldest first.
    pub attachments: Vec<AttachmentView>,
}

/// The task list plus the counts a header shows, so a surface does not fold the
/// same list twice to render both.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TodoListView {
    pub items: Vec<TodoItemView>,
    pub pending: usize,
    pub in_progress: usize,
    pub completed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TodoItemView {
    pub content: String,
    /// `pending`, `in_progress` or `completed`. A string rather than an enum on
    /// the wire, so a surface built against this version renders a status added
    /// later as itself instead of failing to parse the view.
    pub status: String,
}

/// The standing goal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalView {
    /// The compare-and-set token. A surface that offers a "pause" button sends
    /// this back, which is what stops the button acting on a goal that has
    /// moved since it was drawn.
    pub revision: u64,
    pub objective: String,
    /// `active`, `paused`, `blocked` or `complete`.
    pub phase: String,
    /// Present exactly while the phase is `blocked`.
    pub blocker: Option<BlockerView>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BlockerView {
    pub code: String,
    pub message: String,
}

/// Plan mode, and the last plan presented.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanView {
    pub active: bool,
    /// The markdown the model last presented, or `null` if it never has. Not
    /// rendered: a surface renders markdown, and this crate would be guessing
    /// at the width and the theme.
    pub presented: Option<String>,
}

/// What the run has reported to its operator.
///
/// The count and the last entry rather than the whole list, because a panel
/// shows "3 reports" with the newest under it, and a session that reported
/// forty times should not put forty strings in every fold. A surface that wants
/// all of them reads the journal, which is where they are.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FeedbackView {
    pub count: usize,
    pub latest: Option<FeedbackEntryView>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FeedbackEntryView {
    pub text: String,
    pub author: Option<String>,
}

/// One admitted attachment, without its bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttachmentView {
    /// The content address. A surface fetches the bytes by this id, and equal
    /// bytes always carry the same one - so a thumbnail can be cached against
    /// it and two attachments of one screenshot share the cache entry.
    pub id: String,
    pub name: String,
    pub media_type: String,
    /// Size in bytes. A surface deciding whether to inline something needs the
    /// number before it asks for the content.
    pub bytes: usize,
    /// Present for a picture whose header this build could read.
    pub dimensions: Option<DimensionsView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DimensionsView {
    pub width: u32,
    pub height: u32,
}

/// What the harness knows about the project, as a surface reads it.
///
/// Separate from [`SessionView`] because it is not folded from the journal: it
/// is read from the filesystem, it changes when the disk changes rather than
/// when the session does, and a surface refreshes it on a different rhythm.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceView {
    pub root: String,
    /// Where the session is working, when that is not the root.
    pub cwd: Option<String>,
    /// The marker that identified the root - `.git`, `.hg` - or `null` when no
    /// marker was found and the working directory is standing in. A surface
    /// worth its salt says which, because "this is a project" and "this is a
    /// directory" lead a user to different next actions.
    pub marker: Option<String>,
    pub entries: Vec<EntryView>,
    /// Whether the listing was cut short.
    pub truncated: bool,
    /// The instruction files the project keeps, named as the prompt names them.
    pub instructions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EntryView {
    pub name: String,
    pub directory: bool,
}

impl SessionView {
    /// Fold one session's feature state out of its journal.
    ///
    /// One pass over the events for the caller, whatever it is folding: a
    /// surface asking for four panels should not walk the log four times, and
    /// the alternative - four public folds a caller composes - is four chances
    /// to fold a different prefix and render a panel from a different moment.
    pub fn of(events: &[SessionEvent]) -> Self {
        Self {
            as_of_seq: events.last().map_or(-1, |event| event.seq as i64),
            todos: todo::current(events).map(TodoListView::of),
            goal: goal::current(events).map(GoalView::of),
            plan: PlanView {
                active: plan::active(events),
                presented: plan::presented(events),
            },
            feedback: FeedbackView::of(events),
            attachments: attachment::recorded(events)
                .into_iter()
                .map(AttachmentView::of)
                .collect(),
        }
    }
}

impl TodoListView {
    fn of(items: Vec<TodoItem>) -> Self {
        let counts = Counts::of(&items);
        Self {
            items: items.into_iter().map(TodoItemView::of).collect(),
            pending: counts.pending,
            in_progress: counts.in_progress,
            completed: counts.completed,
        }
    }
}

impl TodoItemView {
    fn of(item: TodoItem) -> Self {
        Self {
            status: item.status.as_str().to_string(),
            content: item.content,
        }
    }
}

impl GoalView {
    fn of(goal: Goal) -> Self {
        Self {
            revision: goal.revision,
            objective: goal.objective,
            phase: goal.phase.as_str().to_string(),
            blocker: goal.blocker.map(|blocker| BlockerView {
                code: blocker.code,
                message: blocker.message,
            }),
        }
    }
}

impl FeedbackView {
    fn of(events: &[SessionEvent]) -> Self {
        let entries = feedback::recorded(events);
        Self {
            count: entries.len(),
            latest: entries.last().map(|entry| FeedbackEntryView {
                text: entry.text.clone(),
                author: entry.author.clone(),
            }),
        }
    }
}

impl AttachmentView {
    fn of(attachment: Attachment) -> Self {
        Self {
            id: attachment.id,
            name: attachment.name,
            media_type: attachment.media_type,
            bytes: attachment.bytes,
            dimensions: attachment.dimensions.map(|size| DimensionsView {
                width: size.width,
                height: size.height,
            }),
        }
    }
}

impl WorkspaceView {
    /// Describe a workspace for a surface.
    ///
    /// Paths are rendered as text here rather than left as `PathBuf`, because a
    /// path is not a string on every platform and a surface receiving one over
    /// JSON gets a string anyway - doing it once, here, means one lossy
    /// conversion instead of one per consumer.
    pub fn of(workspace: &Workspace) -> Self {
        Self {
            root: workspace.root.display().to_string(),
            cwd: (workspace.cwd != workspace.root).then(|| workspace.cwd.display().to_string()),
            marker: workspace.marker.clone(),
            entries: workspace
                .entries
                .iter()
                .map(|entry| EntryView {
                    name: entry.name.clone(),
                    directory: entry.directory,
                })
                .collect(),
            truncated: workspace.truncated,
            instructions: workspace.instructions.clone(),
        }
    }
}

/// Where one attachment's bytes are, for a surface that wants to show them.
///
/// A path rather than the content, for the reason the module note gives. The
/// surface that serves a thumbnail reads this file; nothing streams bytes
/// through a view.
///
/// The layout is [`attachment::object_path`]'s, not a second copy of it: a
/// surface that computed the path itself would be a place the store's layout
/// could change out from under.
pub fn attachment_path(store_root: &Path, id: &str) -> PathBuf {
    attachment::object_path(store_root, id)
}
