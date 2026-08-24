//! Test Design Specification: the todo list, ported.
//!
//! Feature under test: `tetanus_features::todo` - the whole-list replacement
//! tool, what it refuses, the durable snapshot it writes, and the fold that is
//! the list. Upstream pins the same decisions in
//! `packages/todo/tool-todo/tests/tool-todo.spec.ts`, its `integration.spec.ts`
//! and its `projection.spec.ts`; each case names the one it restates.
//!
//! Approach: the tool through a `ToolRegistry` against a real `JsonlSessionLog`
//! in a temporary directory, and the fold read back from the file rather than
//! from memory wherever the case is about durability. A list that only exists
//! in a process is exactly the thing this feature must not be.
//!
//! What is not restated, and why. Upstream's projection registry, its
//! `stateVersion`, and its HMR disposal cases are Cordis machinery: the fold
//! here is a function over the log, so there is no registration to drop and no
//! cached state to version. Its "rejects a non-agent caller" case has no
//! counterpart - a tetanus tool is composed with the log it writes to, so there
//! is no callerless dispatch to refuse. Its presentation cases (call title, raw
//! input rendering) belong to the presentation lane by
//! `docs/interface-contract.md` §5.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime. No
//! case reaches a network or an API key.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod support;

use serde_json::json;
use support::Fixture;
use tetanus_features::todo::{
    canonical, current, topic, Counts, Parallelism, Status, TodoError, TodoItem, TodoWriteTool,
};
use tetanus_turn::log::topic as turn_topic;

fn item(content: &str, status: Status) -> TodoItem {
    TodoItem {
        content: content.into(),
        status,
    }
}

fn list(contents: &[(&str, Status)]) -> serde_json::Value {
    json!(contents
        .iter()
        .map(|(content, status)| json!({ "content": content, "status": status.as_str() }))
        .collect::<Vec<_>>())
}

/// TC-PORT-TODO-1: a write appends one snapshot carrying the whole list, and
/// answers the model with the list and its counts.
///
/// Upstream: "appends a todo/write event carrying the whole list to the calling
/// session".
///
/// Input: one call with three tasks in three states.
/// Expected: one `todo/write` on the journal holding all three; the result is
/// `ok` and carries the list plus the counts, so a model that just sent eleven
/// items does not have to count them to know what it said.
#[tokio::test]
async fn a_write_appends_the_whole_list_and_answers_with_its_counts() {
    let h = Fixture::new("write").await;
    h.register(TodoWriteTool::new(h.log(), Parallelism::SingleActive));

    let outcome = h
        .call(
            TodoWriteTool::NAME,
            json!({ "todos": list(&[
                ("read the spec", Status::Completed),
                ("write the fold", Status::InProgress),
                ("port the cases", Status::Pending),
            ]) }),
        )
        .await;

    assert!(outcome.ok, "{}", outcome.content);
    let answered: serde_json::Value = serde_json::from_str(&outcome.content).expect("JSON result");
    assert_eq!(answered["todos"].as_array().expect("todos").len(), 3);
    assert_eq!(answered["counts"]["completed"], 1);
    assert_eq!(answered["counts"]["in_progress"], 1);
    assert_eq!(answered["counts"]["pending"], 1);
    let written = h.events(topic::TODO_WRITE);
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].data["todos"][1]["content"], "write the fold");
    assert_eq!(written[0].data["todos"][1]["status"], "in_progress");
}

/// TC-PORT-TODO-2: the trimmed content is what is stored.
///
/// Upstream: "stores the trimmed content (the dedupe/length key), not the raw
/// input".
///
/// Input: a task whose content is padded with whitespace.
/// Expected: the journal holds the trimmed line. The trimmed form is the
/// identity - it is what is displayed and what duplicate detection compares -
/// so storing the raw input would make two spellings of one task two tasks.
#[tokio::test]
async fn the_trimmed_line_is_what_is_stored() {
    let h = Fixture::new("trim").await;
    h.register(TodoWriteTool::new(h.log(), Parallelism::SingleActive));

    h.call(
        TodoWriteTool::NAME,
        json!({ "todos": [{ "content": "  port the cases \n", "status": "pending" }] }),
    )
    .await;

    assert_eq!(
        h.events(topic::TODO_WRITE)[0].data["todos"][0]["content"],
        "port the cases"
    );
}

/// TC-PORT-TODO-3: a second call replaces the list.
///
/// Upstream: "replaces the list on a second call (last-write-wins on the log)".
///
/// Input: two calls, the second shorter than the first.
/// Expected: both snapshots stay on the journal - it is append-only - and the
/// fold answers the second. The history is what makes a transcript able to show
/// how the plan changed; the fold is what makes the current list unambiguous.
#[tokio::test]
async fn a_second_call_replaces_the_list_and_the_journal_keeps_both() {
    let h = Fixture::new("replace").await;
    h.register(TodoWriteTool::new(h.log(), Parallelism::SingleActive));

    h.call(
        TodoWriteTool::NAME,
        json!({ "todos": list(&[("first", Status::InProgress), ("second", Status::Pending)]) }),
    )
    .await;
    h.call(
        TodoWriteTool::NAME,
        json!({ "todos": list(&[("first", Status::Completed)]) }),
    )
    .await;

    assert_eq!(
        h.events(topic::TODO_WRITE).len(),
        2,
        "the log is append-only"
    );
    let now = current(&h.log().events()).expect("a list");
    assert_eq!(now, vec![item("first", Status::Completed)]);
}

/// TC-PORT-TODO-4: the list survives a reload, because the log is the list.
///
/// Upstream: replay is last-write-wins over the session events.
///
/// Input: two writes, then the journal replayed from disk into the fold.
/// Expected: the same list a live reader sees. This is the acceptance criterion
/// for every feature in this crate: state that a replay cannot reproduce is
/// state the harness would lose.
#[tokio::test]
async fn the_list_survives_a_reload_because_the_journal_is_the_list() {
    let h = Fixture::new("reload").await;
    h.register(TodoWriteTool::new(h.log(), Parallelism::SingleActive));
    h.call(
        TodoWriteTool::NAME,
        json!({ "todos": list(&[("stale", Status::Pending)]) }),
    )
    .await;
    h.call(
        TodoWriteTool::NAME,
        json!({ "todos": list(&[("current", Status::InProgress), ("next", Status::Pending)]) }),
    )
    .await;
    h.flush();

    let replayed = h.replay();

    assert_eq!(
        current(&replayed),
        Some(vec![
            item("current", Status::InProgress),
            item("next", Status::Pending),
        ])
    );
}

/// TC-PORT-TODO-5: the next turn clears the standing list; the end of a turn
/// does not.
///
/// Upstream: "clears the standing plan on the next turn/start (turn/end keeps
/// it)".
///
/// Input: a written list, then `turn/end`, then `turn/start`.
/// Expected: the list still stands after the turn ended, and is gone once the
/// next turn began. A person reading the end of a turn wants the finished
/// checklist; a list from the previous turn shown as the current plan is a plan
/// nobody made.
#[tokio::test]
async fn the_next_turn_clears_the_list_and_the_end_of_a_turn_does_not() {
    let h = Fixture::new("boundary").await;
    h.register(TodoWriteTool::new(h.log(), Parallelism::SingleActive));
    h.call(
        TodoWriteTool::NAME,
        json!({ "todos": list(&[("done", Status::Completed)]) }),
    )
    .await;

    h.append(turn_topic::TURN_END, json!({ "turn": 1 }));
    let after_end = current(&h.log().events());
    h.append(turn_topic::TURN_START, json!({ "turn": 2 }));
    let after_next_start = current(&h.log().events());

    assert_eq!(after_end, Some(vec![item("done", Status::Completed)]));
    assert_eq!(after_next_start, None);
}

/// TC-PORT-TODO-6: before the first write there is no list at all.
///
/// Upstream: "serves null before the first todo/write".
///
/// Input: a journal with a turn on it and no write.
/// Expected: `None`, which is distinct from an empty list - "no plan yet" and
/// "a plan with nothing in it" are different facts, and a surface renders them
/// differently.
#[tokio::test]
async fn before_the_first_write_there_is_no_list_rather_than_an_empty_one() {
    let h = Fixture::new("empty").await;

    assert_eq!(current(&h.log().events()), None);

    h.register(TodoWriteTool::new(h.log(), Parallelism::SingleActive));
    h.call(TodoWriteTool::NAME, json!({ "todos": [] })).await;

    assert_eq!(
        current(&h.log().events()),
        Some(Vec::new()),
        "an emptied list is a list the model emptied"
    );
}

/// TC-PORT-TODO-7: the single-active policy refuses a parallel list, and the
/// parallel policy accepts the very list it refuses.
///
/// Upstream: "false rejects a call marking several items in_progress", "true
/// accepts the very list false rejects", "false still accepts one active item".
///
/// Input: the same two-active list under both policies.
/// Expected: refused under `SingleActive` with the count in the message, and
/// nothing written; accepted under `Parallel`. The refusal names the count
/// because a model told "at most one" and shown how many it sent can fix the
/// list in one step.
#[tokio::test]
async fn the_active_policy_decides_the_very_same_list() {
    let two_active = json!({ "todos": list(&[
        ("one", Status::InProgress),
        ("two", Status::InProgress),
    ]) });

    let single = Fixture::new("single").await;
    single.register(TodoWriteTool::new(single.log(), Parallelism::SingleActive));
    let refused = single.call(TodoWriteTool::NAME, two_active.clone()).await;
    let one_active = single
        .call(
            TodoWriteTool::NAME,
            json!({ "todos": list(&[("one", Status::InProgress), ("two", Status::Pending)]) }),
        )
        .await;

    let parallel = Fixture::new("parallel").await;
    parallel.register(TodoWriteTool::new(parallel.log(), Parallelism::Parallel));
    let accepted = parallel.call(TodoWriteTool::NAME, two_active).await;

    assert!(!refused.ok);
    assert!(
        refused.content.contains("2 tasks are in_progress"),
        "{}",
        refused.content
    );
    assert_eq!(
        single.events(topic::TODO_WRITE).len(),
        1,
        "the refused call wrote nothing; only the accepted one did"
    );
    assert!(one_active.ok);
    assert!(accepted.ok, "{}", accepted.content);
}

/// TC-PORT-TODO-8: the description tells the model the rule the tool enforces.
///
/// Upstream: "instructs the model to keep at most one active, while true
/// instructs parallel".
///
/// Input: both policies' schemas.
/// Expected: each description states its own rule. A model told to mark
/// everything active and then refused for doing it has been set up to fail, so
/// the enforced rule and the advertised rule are asserted together.
#[tokio::test]
async fn the_description_states_the_rule_that_is_enforced() {
    let single = Fixture::new("describe-single").await;
    single.register(TodoWriteTool::new(single.log(), Parallelism::SingleActive));
    let parallel = Fixture::new("describe-parallel").await;
    parallel.register(TodoWriteTool::new(parallel.log(), Parallelism::Parallel));

    let single_text = single.schema(TodoWriteTool::NAME).description;
    let parallel_text = parallel.schema(TodoWriteTool::NAME).description;

    assert!(single_text.contains("AT MOST ONE"), "{single_text}");
    assert!(!single_text.contains("several at once"), "{single_text}");
    assert!(parallel_text.contains("several at once"), "{parallel_text}");
    for text in [&single_text, &parallel_text] {
        assert!(
            text.contains("ENTIRE list"),
            "both say the call replaces the list: {text}"
        );
    }
}

/// TC-PORT-TODO-9: a malformed list is refused before anything is written.
///
/// Upstream: "rejects a malformed status before execute runs (registry
/// arg-validation)" and "rejects a non-array todos argument".
///
/// Input: a status outside the enum, a `todos` that is not an array, an empty
/// content line, and the same line twice.
/// Expected: each is refused and the journal stays empty. The first two are
/// argument errors, because the shape is wrong; the second two come back as
/// failed results the model can correct, because the shape was right and the
/// content was not.
#[tokio::test]
async fn a_malformed_list_is_refused_and_writes_nothing() {
    let h = Fixture::new("malformed").await;
    h.register(TodoWriteTool::new(h.log(), Parallelism::SingleActive));

    let bad_status = h
        .dispatch(
            TodoWriteTool::NAME,
            json!({ "todos": [{ "content": "x", "status": "started" }] }),
        )
        .await;
    let not_a_list = h
        .dispatch(TodoWriteTool::NAME, json!({ "todos": "everything" }))
        .await;
    let blank = h
        .call(
            TodoWriteTool::NAME,
            json!({ "todos": [{ "content": "   ", "status": "pending" }] }),
        )
        .await;
    let duplicated = h
        .call(
            TodoWriteTool::NAME,
            json!({ "todos": list(&[("same", Status::Pending), ("same", Status::Completed)]) }),
        )
        .await;

    assert!(
        bad_status.is_err(),
        "an unknown status is an argument error"
    );
    assert!(not_a_list.is_err());
    assert!(!blank.ok);
    assert!(blank.content.contains("no content"), "{}", blank.content);
    assert!(!duplicated.ok);
    assert!(
        duplicated.content.contains("twice"),
        "{}",
        duplicated.content
    );
    assert!(h.events(topic::TODO_WRITE).is_empty());
}

/// TC-PORT-TODO-10: the canonical form, and the counts, stated directly.
///
/// Upstream's validation helper, restated as a unit so a wiring bug in the tool
/// and a rule bug in the check are distinguishable.
///
/// Input: the four judgements the rule makes, called directly.
/// Expected: trimming, duplicate detection on the trimmed form, the active cap
/// under one policy and not the other, and counts that add up. Every other case
/// in this file goes through the tool; this one is what tells a reader which
/// layer broke when two fail at once.
#[test]
fn the_rule_is_trimming_then_duplicates_then_the_active_cap() {
    let padded = [item("  a  ", Status::Pending), item("b", Status::Completed)];
    let duplicated = [item("a", Status::Pending), item(" a ", Status::Completed)];
    let two_active = [item("a", Status::InProgress), item("b", Status::InProgress)];

    let canonicalized = canonical(&padded, Parallelism::SingleActive).expect("valid");
    assert_eq!(canonicalized[0].content, "a");
    assert_eq!(
        canonical(&duplicated, Parallelism::SingleActive),
        Err(TodoError::Duplicate {
            content: "a".into()
        }),
        "the trimmed form is what duplicate detection compares"
    );
    assert_eq!(
        canonical(&two_active, Parallelism::SingleActive),
        Err(TodoError::TooManyActive { count: 2 })
    );
    assert!(canonical(&two_active, Parallelism::Parallel).is_ok());
    assert_eq!(
        Counts::of(&canonicalized),
        Counts {
            pending: 1,
            in_progress: 0,
            completed: 1,
        }
    );
}

/// TC-PORT-TODO-11: a snapshot this build cannot read leaves the list standing.
///
/// Upstream: "ignores non-todo and malformed todo-shaped events fail-soft".
///
/// Input: a good snapshot, then a `todo/write` whose payload is not a list.
/// Expected: the fold still answers the good one. A journal outlives the build
/// that wrote it, so a record a later build cannot parse must not make the
/// session unreadable - and claiming there is no list when there is one is the
/// worse of the two available answers.
#[tokio::test]
async fn a_snapshot_this_build_cannot_read_leaves_the_previous_list_standing() {
    let h = Fixture::new("fail-soft").await;
    h.register(TodoWriteTool::new(h.log(), Parallelism::SingleActive));
    h.call(
        TodoWriteTool::NAME,
        json!({ "todos": list(&[("real", Status::Pending)]) }),
    )
    .await;

    h.append(topic::TODO_WRITE, json!({ "todos": "from a later build" }));

    assert_eq!(
        current(&h.log().events()),
        Some(vec![item("real", Status::Pending)])
    );
}
