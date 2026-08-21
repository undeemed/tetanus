//! Test Design Specification: the standing goal, ported.
//!
//! Feature under test: `tetanus_features::goal` - the durable objective, the
//! compare-and-set revision every change names, the phase transitions that are
//! legal, the tombstone a clear leaves, and the two tools over them. Upstream
//! pins the same decisions in `packages/goal/goal/tests/goal.spec.ts`, its
//! `projection.spec.ts` and `packages/goal/tool-goal/tests/tool-goal.spec.ts`.
//!
//! Approach: the rules through `decide` where the case is about a rule, and
//! through the tools against a real journal where it is about the record. Both,
//! because a rule that is right and unwired and a wiring that is right over a
//! wrong rule fail differently and a reader should be able to tell which.
//!
//! What is not restated, and why. Upstream's goal-round driver - the autonomous
//! continuation budget, round reservations, staleness against a revision,
//! authority checks distinguishing a human turn from a model one, and the
//! wrap-up instruction - is an autonomy layer over this state, and tetanus has
//! no autonomous continuation loop for it to drive. Its `activation`
//! (`armed`/`disarmed`) is process-local by upstream's own definition and never
//! persisted, so a journal-only restatement has nothing to hold. Its
//! `maxGoalRounds` cap counts rounds that driver admits. Its Remote/Gateway
//! adaptation, its projection registry and its HMR disposal cases are Cordis
//! machinery. `docs/parity-updates/` carries all of it.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod support;

use serde_json::json;
use support::Fixture;
use tetanus_features::goal::{
    current, decide, topic, was_cleared, Blocker, Goal, GoalError, GoalReadTool, GoalWriteTool,
    Operation, Phase,
};

fn goal_at(revision: u64, phase: Phase) -> Goal {
    Goal {
        revision,
        objective: "ship the parser".into(),
        phase,
        blocker: None,
    }
}

async fn with_goal(name: &str) -> Fixture {
    let h = Fixture::new(name).await;
    h.register(GoalReadTool::new(h.log()));
    h.register(GoalWriteTool::new(h.log()));
    h
}

/// TC-PORT-GOAL-1: creating writes one whole-state record at revision one.
///
/// Upstream: "applies the configured default and writes one durable goal
/// change", and every change carries the complete post-change state.
///
/// Input: one create.
/// Expected: one `goal/changed` holding the whole goal, active, at revision
/// one, with the objective trimmed. The record is whole rather than a patch so
/// a reader starting mid-journal still sees a coherent goal.
#[tokio::test]
async fn creating_writes_one_whole_record_at_revision_one() {
    let h = with_goal("create").await;

    let outcome = h
        .call(
            GoalWriteTool::NAME,
            json!({ "action": "create", "objective": "  ship the parser  " }),
        )
        .await;

    assert!(outcome.ok, "{}", outcome.content);
    let written = h.events(topic::GOAL_CHANGED);
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].data["operation"], "create");
    assert_eq!(written[0].data["goal"]["revision"], 1);
    assert_eq!(written[0].data["goal"]["objective"], "ship the parser");
    assert_eq!(written[0].data["goal"]["phase"], "active");
    assert_eq!(current(&h.log().events()), Some(goal_at(1, Phase::Active)));
}

/// TC-PORT-GOAL-2: the goal survives a reload.
///
/// Upstream: the fold is over durable goal events.
///
/// Input: a create and an edit, then the journal replayed from disk.
/// Expected: the same goal a live reader sees, at the same revision. State a
/// replay cannot reproduce is state the harness would lose on restart.
#[tokio::test]
async fn the_goal_survives_a_reload() {
    let h = with_goal("reload").await;
    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "create", "objective": "first" }),
    )
    .await;
    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "edit", "revision": 1, "objective": "ship the parser" }),
    )
    .await;
    h.flush();

    let replayed = h.replay();

    assert_eq!(current(&replayed), Some(goal_at(2, Phase::Active)));
}

/// TC-PORT-GOAL-3: a change naming the wrong revision is refused.
///
/// Upstream: "edits with compare-and-set revisions".
///
/// Input: an edit at revision 1 after the goal has moved to revision 2.
/// Expected: refused, naming both revisions and saying to read again; the goal
/// untouched. A model completing a goal it read three steps ago may be
/// completing an objective nobody set, and nothing else would tell it.
#[tokio::test]
async fn a_change_at_a_stale_revision_is_refused_and_says_what_to_do() {
    let h = with_goal("stale").await;
    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "create", "objective": "ship the parser" }),
    )
    .await;
    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "pause", "revision": 1 }),
    )
    .await;

    let refused = h
        .call(
            GoalWriteTool::NAME,
            json!({ "action": "edit", "revision": 1, "objective": "something else" }),
        )
        .await;

    assert!(!refused.ok);
    assert!(
        refused.content.contains("revision 2, not 1"),
        "{}",
        refused.content
    );
    assert!(
        refused.content.contains("read it again"),
        "{}",
        refused.content
    );
    assert_eq!(
        current(&h.log().events()).map(|goal| goal.phase),
        Some(Phase::Paused)
    );
}

/// TC-PORT-GOAL-4: an unfinished goal is not silently replaced.
///
/// Upstream: "refuses silent replacement of unfinished work", and "allows
/// replacement only after completion".
///
/// Input: a create while an active goal stands; then the same create after the
/// goal is completed.
/// Expected: refused the first time, naming the goal in the way; accepted the
/// second, at the next revision. The refusal is what keeps a model from
/// abandoning work a person asked for by starting something else.
#[tokio::test]
async fn an_unfinished_goal_is_not_replaced_but_a_finished_one_is() {
    let h = with_goal("replace").await;
    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "create", "objective": "ship the parser" }),
    )
    .await;

    let refused = h
        .call(
            GoalWriteTool::NAME,
            json!({ "action": "create", "objective": "rewrite the renderer" }),
        )
        .await;
    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "complete", "revision": 1 }),
    )
    .await;
    let accepted = h
        .call(
            GoalWriteTool::NAME,
            json!({ "action": "create", "objective": "rewrite the renderer" }),
        )
        .await;

    assert!(!refused.ok);
    assert!(
        refused.content.contains("ship the parser"),
        "{}",
        refused.content
    );
    assert!(accepted.ok, "{}", accepted.content);
    let now = current(&h.log().events()).expect("a goal");
    assert_eq!(now.objective, "rewrite the renderer");
    assert_eq!(
        now.revision, 3,
        "the revision keeps counting across the pair"
    );
}

/// TC-PORT-GOAL-5: the phase transitions, and the ones that are refused.
///
/// Upstream: "supports pause, resume, block, and completion transitions",
/// "allows completion from every stopped phase", and "rejects invalid phase
/// transitions".
///
/// Input: each move against each phase, through `decide` so the table is read
/// without a journal.
/// Expected: pause from active or blocked; resume from paused or blocked;
/// completion from every unfinished phase, because a goal that turned out to be
/// done while paused is done and forcing a resume first would put a lie on the
/// journal; and nothing at all out of `complete` except a new goal.
#[test]
fn the_phase_table_is_what_it_says() {
    let legal = |phase: Phase, operation: Operation| {
        decide(Some(goal_at(1, phase)), &operation).map(|goal| goal.phase)
    };

    assert_eq!(
        legal(Phase::Active, Operation::Pause { revision: 1 }),
        Ok(Phase::Paused)
    );
    assert_eq!(
        legal(Phase::Blocked, Operation::Pause { revision: 1 }),
        Ok(Phase::Paused)
    );
    assert!(legal(Phase::Paused, Operation::Pause { revision: 1 }).is_err());
    assert_eq!(
        legal(Phase::Paused, Operation::Resume { revision: 1 }),
        Ok(Phase::Active)
    );
    assert_eq!(
        legal(Phase::Blocked, Operation::Resume { revision: 1 }),
        Ok(Phase::Active)
    );
    assert!(legal(Phase::Active, Operation::Resume { revision: 1 }).is_err());
    for phase in [Phase::Active, Phase::Paused, Phase::Blocked] {
        assert_eq!(
            legal(phase, Operation::Complete { revision: 1 }),
            Ok(Phase::Complete),
            "a goal that is done is done, whatever phase it was resting in"
        );
    }
    assert_eq!(
        legal(Phase::Complete, Operation::Complete { revision: 1 }),
        Err(GoalError::BadTransition {
            from: "complete",
            to: "complete"
        })
    );
    assert!(
        legal(
            Phase::Complete,
            Operation::Edit {
                revision: 1,
                objective: "again".into()
            }
        )
        .is_err(),
        "a finished goal is not edited back to life; that is a new goal"
    );
    assert!(
        legal(Phase::Complete, Operation::Clear { revision: 1 }).is_ok(),
        "clearing is the way out of any state"
    );
}

/// TC-PORT-GOAL-6: blocking records a reason, and resuming drops it.
///
/// Upstream: "records canonical blocker reasons".
///
/// Input: a block with a code and a message; then a resume; then a block with
/// an empty reason.
/// Expected: the reason is carried while blocked and gone once resumed, and a
/// blocked goal with no reason is refused. A session that has stopped for no
/// stated reason is the thing this prevents.
#[tokio::test]
async fn blocking_carries_a_reason_and_resuming_drops_it() {
    let h = with_goal("blocked").await;
    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "create", "objective": "ship the parser" }),
    )
    .await;

    let blocked = h
        .call(
            GoalWriteTool::NAME,
            json!({
                "action": "block",
                "revision": 1,
                "blocker_code": "needs-credential",
                "blocker_message": "the deploy key is not on this machine",
            }),
        )
        .await;
    let carried = current(&h.log().events()).expect("a goal");
    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "resume", "revision": 2 }),
    )
    .await;
    let resumed = current(&h.log().events()).expect("a goal");
    let unexplained = h
        .call(
            GoalWriteTool::NAME,
            json!({ "action": "block", "revision": 3, "blocker_code": "", "blocker_message": "" }),
        )
        .await;

    assert!(blocked.ok, "{}", blocked.content);
    assert_eq!(
        carried.blocker,
        Some(Blocker {
            code: "needs-credential".into(),
            message: "the deploy key is not on this machine".into(),
        })
    );
    assert_eq!(carried.phase, Phase::Blocked);
    assert_eq!(resumed.phase, Phase::Active);
    assert_eq!(resumed.blocker, None);
    assert!(!unexplained.ok);
    assert!(
        unexplained.content.contains("needs a reason"),
        "{}",
        unexplained.content
    );
}

/// TC-PORT-GOAL-7: clearing leaves a tombstone, and a new goal may follow.
///
/// Upstream: "clears through a revisioned tombstone and permits a fresh goal",
/// and "returns to null after a clear tombstone".
///
/// Input: a create, a clear, a read, then another create.
/// Expected: the fold answers no goal; the journal still says one was dropped,
/// which is a different fact from never having had one; and the next create is
/// accepted. A surface showing "no goal" for a session whose goal was abandoned
/// would be hiding a decision somebody made.
#[tokio::test]
async fn clearing_leaves_a_tombstone_that_a_later_goal_replaces() {
    let h = with_goal("clear").await;
    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "create", "objective": "ship the parser" }),
    )
    .await;

    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "clear", "revision": 1 }),
    )
    .await;
    let after_clear = h.call(GoalReadTool::NAME, json!({})).await;
    let events = h.log().events();
    let fresh = h
        .call(
            GoalWriteTool::NAME,
            json!({ "action": "create", "objective": "rewrite the renderer" }),
        )
        .await;

    assert_eq!(current(&events), None);
    assert!(was_cleared(&events), "the journal says it was dropped");
    let read: serde_json::Value = serde_json::from_str(&after_clear.content).expect("JSON");
    assert_eq!(read["goal"], serde_json::Value::Null);
    assert_eq!(read["cleared"], true);
    assert!(fresh.ok, "{}", fresh.content);
}

/// TC-PORT-GOAL-8: reading before anything is set says so without a tombstone.
///
/// Upstream: "serves null before the first create".
///
/// Input: a read on a journal with no goal.
/// Expected: no goal and `cleared: false`. Two absences that mean different
/// things, answered differently.
#[tokio::test]
async fn reading_before_a_goal_exists_reports_no_goal_and_no_tombstone() {
    let h = with_goal("empty").await;

    let read = h.call(GoalReadTool::NAME, json!({})).await;

    let parsed: serde_json::Value = serde_json::from_str(&read.content).expect("JSON");
    assert!(read.ok);
    assert_eq!(parsed["goal"], serde_json::Value::Null);
    assert_eq!(parsed["cleared"], false);
}

/// TC-PORT-GOAL-9: an action that needs a revision or an objective says which.
///
/// Upstream: "returns structured domain and conditional-argument failures", and
/// "accepts only empty fillers in fields unused by the selected action".
///
/// Input: an edit with no revision, a create with no objective, and an action
/// that is not an action.
/// Expected: each comes back as a failed result naming the argument the chosen
/// action needed. Conditional arguments are why the reader is hand-written: a
/// model that omitted one should be told which one.
#[tokio::test]
async fn a_missing_conditional_argument_names_itself() {
    let h = with_goal("arguments").await;

    let no_revision = h
        .call(
            GoalWriteTool::NAME,
            json!({ "action": "edit", "objective": "x" }),
        )
        .await;
    let no_objective = h
        .call(GoalWriteTool::NAME, json!({ "action": "create" }))
        .await;
    let not_an_action = h
        .call(GoalWriteTool::NAME, json!({ "action": "obliterate" }))
        .await;

    assert!(
        no_revision.content.contains("`revision`"),
        "{}",
        no_revision.content
    );
    assert!(
        no_objective.content.contains("`objective`"),
        "{}",
        no_objective.content
    );
    assert!(
        not_an_action.content.contains("is not an action"),
        "{}",
        not_an_action.content
    );
    for outcome in [&no_revision, &no_objective, &not_an_action] {
        assert!(!outcome.ok);
    }
    assert!(h.events(topic::GOAL_CHANGED).is_empty());
}

/// TC-PORT-GOAL-10: changing a goal that does not exist is refused as such.
///
/// Upstream: "returns direct missing-state results for pause, resume, and
/// clear".
///
/// Input: a pause with no goal on the journal.
/// Expected: refused, saying to create one first, and nothing written. It is a
/// distinct message from the stale-revision one because the useful next move is
/// different.
#[tokio::test]
async fn changing_a_goal_that_does_not_exist_says_to_create_one() {
    let h = with_goal("missing").await;

    let refused = h
        .call(
            GoalWriteTool::NAME,
            json!({ "action": "pause", "revision": 1 }),
        )
        .await;

    assert!(!refused.ok);
    assert!(
        refused.content.contains("no goal to pause"),
        "{}",
        refused.content
    );
    assert!(
        refused.content.contains("create one first"),
        "{}",
        refused.content
    );
    assert!(h.events(topic::GOAL_CHANGED).is_empty());
}

/// TC-PORT-GOAL-11: an empty objective is refused wherever it appears.
///
/// Upstream: "requires an objective", and "rejects empty edits".
///
/// Input: a create and an edit, each with whitespace for an objective.
/// Expected: both refused, and nothing written. A goal with no objective is a
/// session that cannot tell whether it is finished.
#[tokio::test]
async fn an_empty_objective_is_refused_on_create_and_on_edit() {
    let h = with_goal("blank").await;

    let created = h
        .call(
            GoalWriteTool::NAME,
            json!({ "action": "create", "objective": "   " }),
        )
        .await;
    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "create", "objective": "ship the parser" }),
    )
    .await;
    let edited = h
        .call(
            GoalWriteTool::NAME,
            json!({ "action": "edit", "revision": 1, "objective": "\n" }),
        )
        .await;

    assert!(!created.ok);
    assert!(!edited.ok);
    assert!(
        edited.content.contains("needs an objective"),
        "{}",
        edited.content
    );
    assert_eq!(
        h.events(topic::GOAL_CHANGED).len(),
        1,
        "only the valid create was written"
    );
}

/// TC-PORT-GOAL-12: a record this build cannot read leaves the goal standing.
///
/// Upstream: "ignores non-goal and malformed goal-shaped events fail-soft".
///
/// Input: a good goal, then a `goal/changed` whose payload is not a goal.
/// Expected: the fold still answers the good one. Same rule as the todo fold,
/// for the same reason: a journal outlives the build that wrote it.
#[tokio::test]
async fn a_record_this_build_cannot_read_leaves_the_goal_standing() {
    let h = with_goal("fail-soft").await;
    h.call(
        GoalWriteTool::NAME,
        json!({ "action": "create", "objective": "ship the parser" }),
    )
    .await;

    h.append(
        topic::GOAL_CHANGED,
        json!({ "operation": "edit", "goal": "from a later build" }),
    );

    assert_eq!(current(&h.log().events()), Some(goal_at(1, Phase::Active)));
}
