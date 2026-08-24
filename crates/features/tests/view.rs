//! Test Design Specification: the surface vocabulary.
//!
//! Feature under test: `tetanus_features::view` - the shapes a surface reads,
//! their field names on the wire, and the folding that produces them. This is
//! not a port: upstream's client reads a generated protocol, and these are the
//! types the web UI crew asked for so they build against a vocabulary instead
//! of guessing at one.
//!
//! Approach: fold a real journal and assert the *JSON*, not the struct. A case
//! that asserted the struct would pass while a rename broke every consumer; the
//! field names are the contract, so the field names are what is pinned.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod support;

use serde_json::json;
use support::Fixture;
use tetanus_features::attachment::{attach, Incoming, Limits};
use tetanus_features::feedback;
use tetanus_features::goal::{commit, Blocker, Operation};
use tetanus_features::plan;
use tetanus_features::todo::{Parallelism, TodoWriteTool};
use tetanus_features::view::{SessionView, WorkspaceView};
use tetanus_features::workspace::describe;

fn as_json(view: &SessionView) -> serde_json::Value {
    serde_json::to_value(view).expect("a view serializes")
}

/// TC-VIEW-1: an untouched session folds to a view that says so, without
/// inventing anything.
///
/// Input: a journal with only a turn on it.
/// Expected: no todos, no goal, plan mode off, no feedback, no attachments -
/// and `todos: null` rather than an empty list, because "no plan yet" and "a
/// plan the model emptied" are different facts and a panel renders them
/// differently.
#[tokio::test]
async fn an_untouched_session_folds_to_an_empty_view() {
    let h = Fixture::new("empty").await;

    let view = SessionView::of(&h.log().events());

    let json = as_json(&view);
    assert_eq!(json["todos"], serde_json::Value::Null);
    assert_eq!(json["goal"], serde_json::Value::Null);
    assert_eq!(json["plan"], json!({ "active": false, "presented": null }));
    assert_eq!(json["feedback"], json!({ "count": 0, "latest": null }));
    assert_eq!(json["attachments"], json!([]));
}

/// TC-VIEW-2: the view names the journal position it folded.
///
/// Input: a view taken, then an event appended, then another view.
/// Expected: `as_of_seq` moves with the log, and is -1 for an empty one. A live
/// panel receiving two views out of order has to be able to tell which is
/// newer, and a panel showing a stale one has to be able to say so.
#[tokio::test]
async fn the_view_says_how_far_it_folded() {
    let h = Fixture::bare("seq");
    let empty = SessionView::of(&h.log().events());

    h.append(tetanus_turn::log::topic::TURN_START, json!({ "turn": 1 }));
    let first = SessionView::of(&h.log().events());
    feedback::record(h.log().as_ref(), "something", None).expect("recorded");
    let second = SessionView::of(&h.log().events());

    assert_eq!(empty.as_of_seq, -1, "an empty log has folded nothing");
    assert!(second.as_of_seq > first.as_of_seq);
    assert_eq!(second.as_of_seq, h.log().events().len() as i64 - 1);
}

/// TC-VIEW-3: the todo list arrives with the counts a header needs.
///
/// Input: three tasks in three states.
/// Expected: the items in order with their statuses as strings, and the three
/// counts beside them. The counts are in the view so a surface does not fold
/// the same list twice to draw a header and a body, and the status is a string
/// so a status added later renders as itself instead of failing to parse the
/// whole view.
#[tokio::test]
async fn the_todo_list_arrives_with_its_counts() {
    let h = Fixture::new("todos").await;
    h.register(TodoWriteTool::new(h.log(), Parallelism::SingleActive));
    h.call(
        TodoWriteTool::NAME,
        json!({ "todos": [
            { "content": "read the spec", "status": "completed" },
            { "content": "write the fold", "status": "in_progress" },
            { "content": "port the cases", "status": "pending" },
        ] }),
    )
    .await;

    let json = as_json(&SessionView::of(&h.log().events()));

    assert_eq!(
        json["todos"],
        json!({
            "items": [
                { "content": "read the spec", "status": "completed" },
                { "content": "write the fold", "status": "in_progress" },
                { "content": "port the cases", "status": "pending" },
            ],
            "pending": 1,
            "in_progress": 1,
            "completed": 1,
        })
    );
}

/// TC-VIEW-4: the goal arrives with the revision a button has to send back.
///
/// Input: a goal created and then blocked.
/// Expected: the objective, the phase as a string, the blocker, and the
/// revision. The revision is the load-bearing field: a surface that draws a
/// "resume" button and sends the revision it drew is refused if the goal moved
/// underneath it, which is the difference between a stale click doing nothing
/// and a stale click acting on something else.
#[tokio::test]
async fn the_goal_arrives_with_the_revision_a_button_sends_back() {
    let h = Fixture::new("goal").await;
    commit(
        h.log().as_ref(),
        Operation::Create {
            objective: "ship the parser".into(),
        },
    )
    .expect("created");
    commit(
        h.log().as_ref(),
        Operation::Block {
            revision: 1,
            blocker: Blocker {
                code: "needs-credential".into(),
                message: "the deploy key is not on this machine".into(),
            },
        },
    )
    .expect("blocked");

    let json = as_json(&SessionView::of(&h.log().events()));

    assert_eq!(
        json["goal"],
        json!({
            "revision": 2,
            "objective": "ship the parser",
            "phase": "blocked",
            "blocker": {
                "code": "needs-credential",
                "message": "the deploy key is not on this machine",
            },
        })
    );
}

/// TC-VIEW-5: plan mode and the plan it presented are both in the view, and
/// the markdown is not rendered.
///
/// Input: plan mode on, a plan presented, plan mode on again.
/// Expected: `active: true` with the plan's markdown exactly as the model
/// wrote it. Rendering it here would mean guessing at a width and a theme this
/// crate cannot see; the surface has both.
#[tokio::test]
async fn plan_mode_and_its_markdown_arrive_unrendered() {
    let h = Fixture::new("plan").await;
    plan::set(h.log().as_ref(), true).expect("on");
    h.append(
        plan::topic::PLAN_PRESENTED,
        json!({ "plan": "1. read it\n2. **rewrite** it\n" }),
    );

    let json = as_json(&SessionView::of(&h.log().events()));

    assert_eq!(
        json["plan"],
        json!({ "active": true, "presented": "1. read it\n2. **rewrite** it\n" })
    );
}

/// TC-VIEW-6: feedback arrives as a count and the newest entry.
///
/// Input: three remarks.
/// Expected: the count, and the last one. A session that reported forty times
/// should not put forty strings in every fold a live panel receives, and a
/// surface that wants all of them reads the journal, which is where they are.
#[tokio::test]
async fn feedback_arrives_as_a_count_and_the_newest_entry() {
    let h = Fixture::new("feedback").await;
    for text in ["first", "second", "third"] {
        feedback::record(h.log().as_ref(), text, Some("model")).expect("recorded");
    }

    let json = as_json(&SessionView::of(&h.log().events()));

    assert_eq!(
        json["feedback"],
        json!({
            "count": 3,
            "latest": { "text": "third", "author": "model" },
        })
    );
}

/// TC-VIEW-7: an attachment is described and never carried.
///
/// Input: a text file and an image attached.
/// Expected: id, name, media type, size and dimensions - and no field anywhere
/// in the view holding content. A base64 screenshot inside a push frame is a
/// frame nobody can read and a memory spike on every subscriber; the id is how
/// a surface fetches the bytes once.
#[tokio::test]
async fn an_attachment_is_described_and_never_carried() {
    let h = Fixture::new("attachments").await;
    let store = h.path().parent().expect("parent").join("objects");
    let png = {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&320u32.to_be_bytes());
        bytes.extend_from_slice(&200u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    };
    attach(
        h.log().as_ref(),
        &store,
        &[
            Incoming {
                name: "log.txt".into(),
                media_type: "text/plain".into(),
                bytes: b"a line".to_vec(),
            },
            Incoming {
                name: "shot.png".into(),
                media_type: "image/png".into(),
                bytes: png.clone(),
            },
        ],
        &Limits::default(),
    )
    .expect("attached");

    let view = SessionView::of(&h.log().events());
    let json = as_json(&view);

    let attachments = json["attachments"].as_array().expect("a list");
    assert_eq!(attachments.len(), 2);
    assert_eq!(attachments[0]["name"], "log.txt");
    assert_eq!(attachments[0]["bytes"], 6);
    assert_eq!(attachments[0]["dimensions"], serde_json::Value::Null);
    assert_eq!(
        attachments[1]["dimensions"],
        json!({ "width": 320, "height": 200 })
    );
    let whole = serde_json::to_string(&view).expect("serializes");
    assert!(
        !whole.contains("a line"),
        "no content reaches the view: {whole}"
    );
    // The id is what a surface fetches by, and it is where the bytes are.
    let id = attachments[1]["id"].as_str().expect("an id");
    assert_eq!(
        std::fs::read(tetanus_features::view::attachment_path(&store, id)).expect("read"),
        png
    );
}

/// TC-VIEW-8: the workspace view says which of the two things it is looking at.
///
/// Input: a project with a marker, described from a subdirectory.
/// Expected: the root, the working directory separately, the marker named, the
/// entries, and the instruction files. `cwd` is `null` when it equals the root,
/// so a panel does not draw the same path twice; the marker is `null` when
/// there was none, which is how a surface tells "this is a project" from "this
/// is a directory" - they lead a user to different next actions.
#[test]
fn the_workspace_view_distinguishes_a_project_from_a_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    std::fs::create_dir_all(root.join("project/.git")).expect("marker");
    std::fs::create_dir_all(root.join("project/src")).expect("src");
    std::fs::write(root.join("project/AGENTS.md"), "Work carefully.").expect("instructions");
    let deep = root.join("project/src");

    let view = WorkspaceView::of(&describe(&deep).expect("described"));
    let json = serde_json::to_value(&view).expect("serializes");

    assert_eq!(json["root"], root.join("project").display().to_string());
    assert_eq!(json["cwd"], deep.display().to_string());
    assert_eq!(json["marker"], ".git");
    assert_eq!(json["instructions"], json!(["AGENTS.md"]));
    assert_eq!(
        json["entries"],
        json!([{ "name": "src", "directory": true },
                                       { "name": "AGENTS.md", "directory": false }])
    );
    assert_eq!(json["truncated"], false);

    let at_root = WorkspaceView::of(&describe(&root.join("project")).expect("described"));
    assert_eq!(
        serde_json::to_value(&at_root).expect("serializes")["cwd"],
        serde_json::Value::Null,
        "the working directory is not drawn twice"
    );
}

/// TC-VIEW-9: a view round-trips through JSON.
///
/// Input: a fully populated view, serialized and read back.
/// Expected: the same value. A surface receives these over a wire, so a type
/// that serializes but does not deserialize would be a vocabulary only one side
/// speaks - and the case that catches it is the one that goes both ways.
#[tokio::test]
async fn a_view_survives_the_wire_in_both_directions() {
    let h = Fixture::new("roundtrip").await;
    h.register(TodoWriteTool::new(h.log(), Parallelism::SingleActive));
    h.call(
        TodoWriteTool::NAME,
        json!({ "todos": [{ "content": "do it", "status": "in_progress" }] }),
    )
    .await;
    commit(
        h.log().as_ref(),
        Operation::Create {
            objective: "ship it".into(),
        },
    )
    .expect("created");
    feedback::record(h.log().as_ref(), "a remark", None).expect("recorded");
    plan::set(h.log().as_ref(), true).expect("on");

    let view = SessionView::of(&h.log().events());
    let text = serde_json::to_string(&view).expect("serializes");
    let back: SessionView = serde_json::from_str(&text).expect("deserializes");

    assert_eq!(back, view);
    assert_eq!(back.todos.expect("a list").items[0].status, "in_progress");
}

/// TC-VIEW-10: the fold is one pass over the journal, and every panel in one
/// view describes the same moment.
///
/// Input: a session whose state changes between two folds.
/// Expected: within a view, the todo list, the goal and the plan all come from
/// the same prefix - asserted by folding, changing three things, and folding
/// again, then checking that the first view moved in none of them. Four public
/// folds a caller composed would be four chances to render a panel from a
/// different moment, which is how a UI shows a goal that was completed beside a
/// task list from before it was.
#[tokio::test]
async fn every_panel_in_one_view_describes_one_moment() {
    let h = Fixture::new("coherent").await;
    h.register(TodoWriteTool::new(h.log(), Parallelism::SingleActive));
    h.call(
        TodoWriteTool::NAME,
        json!({ "todos": [{ "content": "before", "status": "pending" }] }),
    )
    .await;
    commit(
        h.log().as_ref(),
        Operation::Create {
            objective: "before".into(),
        },
    )
    .expect("created");

    let before = SessionView::of(&h.log().events());
    h.call(
        TodoWriteTool::NAME,
        json!({ "todos": [{ "content": "after", "status": "completed" }] }),
    )
    .await;
    commit(h.log().as_ref(), Operation::Complete { revision: 1 }).expect("completed");
    plan::set(h.log().as_ref(), true).expect("on");
    let after = SessionView::of(&h.log().events());

    assert_eq!(before.todos.expect("a list").items[0].content, "before");
    assert_eq!(before.goal.expect("a goal").phase, "active");
    assert!(!before.plan.active);
    assert_eq!(after.todos.expect("a list").items[0].content, "after");
    assert_eq!(after.goal.expect("a goal").phase, "complete");
    assert!(after.plan.active);
    assert!(after.as_of_seq > before.as_of_seq);
}
