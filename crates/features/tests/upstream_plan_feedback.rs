//! Test Design Specification: plan mode and operator feedback, ported.
//!
//! Features under test: `tetanus_features::plan` - the mode folded from the
//! log, the guidance section it adds, and the tool that presents a plan and
//! leaves; and `tetanus_features::feedback` - the append-only channel a run
//! reports on. Upstream pins them in
//! `packages/plan/plan-mode/tests/integration.spec.ts` and
//! `packages/feedback/command-feedback/tests/command-feedback.spec.ts`.
//!
//! Approach: the folds and the tools against a real journal, and the guidance
//! section through a real `PromptRegistry` assembly - a case that asserted the
//! section's text without assembling it would not notice a section that never
//! renders.
//!
//! What is not restated, and why. Upstream defers a user's mode flip to the
//! next accepted pre-step so it lands on a step boundary; tetanus records a
//! flip when it is made, because the deferral exists to keep one step's
//! assembly stable and a tetanus assembly is built once per step from the log
//! as it stands - there is no window in which a mid-step flip could be
//! observed. Its `/plan` slash command and its user-review presentation are
//! surfaces. Upstream's `message-feedback` is a per-message rating over a
//! durable store this workspace does not have, and its telemetry disclosure
//! sentences are a deployment's text rather than a rule.
//!
//! Environmental needs: a writable temporary directory and a Tokio runtime.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Fixture;
use tetanus_features::feedback::{self, recorded, FeedbackTool};
use tetanus_features::plan::{self, active, presented, ExitPlanModeTool, SECTION};
use tetanus_turn::prompt::{AssembleAt, PromptRegistry};

const GUIDANCE: &str = "You are in plan mode: work out what you would do and present it.";

/// The text the assembly renders for the `plan:policy` section right now.
fn rendered(prompt: &Arc<PromptRegistry>) -> String {
    prompt
        .assemble(&AssembleAt { turn: 1, step: 1 })
        .into_iter()
        .find(|section| section.id == SECTION)
        .map(|section| section.text)
        .unwrap_or_default()
}

/// TC-PORT-PLAN-1: the mode is a fold, and an empty journal is inactive.
///
/// Upstream: "the state in force is folded from the session log (`plan/mode`,
/// last one wins)"; a prefix with none is inactive.
///
/// Input: a journal with nothing, then on, then off, then on again.
/// Expected: inactive, active, inactive, active. Planning is the mode a
/// deployment opts into rather than one it has to opt out of, and the fold is
/// the whole state so no live mirror can disagree with it.
#[tokio::test]
async fn the_mode_is_the_last_record_and_no_record_means_off() {
    let h = Fixture::new("mode").await;

    assert!(!active(&h.log().events()));
    plan::set(h.log().as_ref(), true).expect("on");
    assert!(active(&h.log().events()));
    plan::set(h.log().as_ref(), false).expect("off");
    assert!(!active(&h.log().events()));
    plan::set(h.log().as_ref(), true).expect("on again");
    assert!(active(&h.log().events()));
}

/// TC-PORT-PLAN-2: setting the mode it is already in writes nothing.
///
/// Upstream's whole-value replace, with the idempotence the approval policy
/// also has.
///
/// Input: two identical sets, then a change.
/// Expected: one record for the first set, none for the repeat, one for the
/// change. A journal full of records that changed nothing makes the ones that
/// did hard to find.
#[tokio::test]
async fn setting_the_mode_it_is_already_in_appends_nothing() {
    let h = Fixture::new("idempotent").await;

    let first = plan::set(h.log().as_ref(), true).expect("on");
    let repeat = plan::set(h.log().as_ref(), true).expect("on again");
    let changed = plan::set(h.log().as_ref(), false).expect("off");

    assert!(first);
    assert!(!repeat);
    assert!(changed);
    assert_eq!(h.events(plan::topic::PLAN_MODE).len(), 2);
}

/// TC-PORT-PLAN-3: the guidance section renders only while the mode is on, and
/// the section is registered either way.
///
/// Upstream: "a deployment-owned guidance section is included in each model
/// request" while active, and entering or leaving "changes only the prompt
/// section, not the request tool catalog".
///
/// Input: one registration, assembled with the mode off, on, and off again.
/// Expected: empty, the guidance, empty. It renders empty rather than being
/// unregistered, because an empty section is dropped by the assembly anyway and
/// a registration that comes and goes is a prompt whose shape changes under the
/// model for a reason it cannot see.
#[tokio::test]
async fn the_guidance_renders_only_while_the_mode_is_on() {
    let h = Fixture::new("guidance").await;
    let prompt = PromptRegistry::new();
    let _handle = plan::guidance(&prompt, h.log(), GUIDANCE, 100).expect("registered");

    let off = rendered(&prompt);
    plan::set(h.log().as_ref(), true).expect("on");
    let on = rendered(&prompt);
    plan::set(h.log().as_ref(), false).expect("off");
    let off_again = rendered(&prompt);

    assert_eq!(off, "");
    assert_eq!(on, GUIDANCE);
    assert_eq!(off_again, "");
}

/// TC-PORT-PLAN-4: the section goes when its registration is dropped.
///
/// Upstream's HMR disposal case, restated against the effect handle that is
/// tetanus's equivalent.
///
/// Input: the guidance registered while the mode is on, then the handle
/// dropped.
/// Expected: the section is in the assembly, then is not. The handle is the
/// registration, so a composer that stops offering plan mode takes its prompt
/// contribution with it.
#[tokio::test]
async fn dropping_the_handle_takes_the_section_out_of_the_assembly() {
    let h = Fixture::new("dispose").await;
    let prompt = PromptRegistry::new();
    plan::set(h.log().as_ref(), true).expect("on");
    let handle = plan::guidance(&prompt, h.log(), GUIDANCE, 100).expect("registered");

    let while_registered = rendered(&prompt);
    drop(handle);
    let after_drop = prompt
        .assemble(&AssembleAt { turn: 1, step: 1 })
        .into_iter()
        .any(|section| section.id == SECTION);

    assert_eq!(while_registered, GUIDANCE);
    assert!(!after_drop);
}

/// TC-PORT-PLAN-5: leaving records the plan and turns the mode off in one
/// place.
///
/// Upstream: "`exit_plan_mode` presents the completed plan for user review".
///
/// Input: the tool called with a plan while the mode is on.
/// Expected: the plan on the journal, the mode off, and the fold able to answer
/// what was presented. A plan kept only in the conversation is one a transcript
/// reader has to reconstruct from prose.
#[tokio::test]
async fn leaving_plan_mode_records_the_plan_and_turns_the_mode_off() {
    let h = Fixture::new("exit").await;
    h.register(ExitPlanModeTool::new(h.log()));
    plan::set(h.log().as_ref(), true).expect("on");

    let outcome = h
        .call(
            ExitPlanModeTool::NAME,
            json!({ "plan": "1. read the parser\n2. rewrite the lexer\n" }),
        )
        .await;

    assert!(outcome.ok, "{}", outcome.content);
    assert!(!active(&h.log().events()));
    assert_eq!(
        presented(&h.log().events()).as_deref(),
        Some("1. read the parser\n2. rewrite the lexer")
    );
}

/// TC-PORT-PLAN-6: the exit tool stays callable when the mode is off, and an
/// empty plan is refused.
///
/// Upstream: "the exit tool remains registered while plan mode is inactive".
///
/// Input: the tool called with the mode off, and with a blank plan while on.
/// Expected: the first is answered plainly rather than refused - nothing is
/// wrong, there is simply no mode to leave - and the second is refused with a
/// reason, leaving the mode on and nothing recorded.
#[tokio::test]
async fn the_exit_tool_is_callable_with_the_mode_off_and_refuses_an_empty_plan() {
    let h = Fixture::new("exit-edges").await;
    h.register(ExitPlanModeTool::new(h.log()));

    let while_off = h
        .call(ExitPlanModeTool::NAME, json!({ "plan": "something" }))
        .await;
    plan::set(h.log().as_ref(), true).expect("on");
    let blank = h
        .call(ExitPlanModeTool::NAME, json!({ "plan": "  " }))
        .await;

    assert!(while_off.ok);
    assert!(
        while_off.content.contains("not on"),
        "{}",
        while_off.content
    );
    assert!(!blank.ok);
    assert!(
        blank.content.contains("nothing to review"),
        "{}",
        blank.content
    );
    assert!(active(&h.log().events()), "the refusal left the mode alone");
    assert!(h.events(plan::topic::PLAN_PRESENTED).is_empty());
}

/// TC-PORT-PLAN-7: the mode and the plan survive a reload.
///
/// Upstream: resume and fork restore the state without a live mirror.
///
/// Input: mode on, a plan presented, mode on again, then a replay from disk.
/// Expected: the cold journal folds to the same mode and the same plan. This is
/// the acceptance criterion every feature in this crate is held to.
#[tokio::test]
async fn the_mode_and_the_plan_survive_a_reload() {
    let h = Fixture::new("plan-reload").await;
    h.register(ExitPlanModeTool::new(h.log()));
    plan::set(h.log().as_ref(), true).expect("on");
    h.call(ExitPlanModeTool::NAME, json!({ "plan": "the plan" }))
        .await;
    plan::set(h.log().as_ref(), true).expect("on again");
    h.flush();

    let replayed = h.replay();

    assert!(active(&replayed));
    assert_eq!(presented(&replayed).as_deref(), Some("the plan"));
}

/// TC-PORT-FEED-1: each remark is its own record, in the order it was made.
///
/// Upstream: "records each entry separately without replacing earlier ones",
/// and "records concurrent submissions in dispatch order".
///
/// Input: three remarks.
/// Expected: three records, read back oldest first. This is the one feature
/// here whose fold is a list: two remarks are two facts, and the second does
/// not replace the first.
#[tokio::test]
async fn every_remark_is_its_own_record_in_the_order_it_was_made() {
    let h = Fixture::new("entries").await;

    for text in ["the first thing", "the second thing", "the third thing"] {
        feedback::record(h.log().as_ref(), text, None).expect("recorded");
    }

    let entries = recorded(&h.log().events());
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].text, "the first thing");
    assert_eq!(entries[2].text, "the third thing");
}

/// TC-PORT-FEED-2: surrounding whitespace is discarded and nothing else is
/// touched.
///
/// Upstream: "normalizes surrounding whitespace without parsing command-like
/// content".
///
/// Input: a padded remark, and one that looks like a command.
/// Expected: trimmed, and otherwise stored exactly - slashes, markup and inner
/// newlines intact. A channel that interpreted what it was given could not be
/// used to report the thing the person is actually looking at.
#[tokio::test]
async fn whitespace_is_normalized_and_content_is_never_parsed() {
    let h = Fixture::new("normalize").await;

    feedback::record(h.log().as_ref(), "  padded  \n", None).expect("recorded");
    feedback::record(h.log().as_ref(), "/plan off <tag> **bold**", None).expect("recorded");

    let entries = recorded(&h.log().events());
    assert_eq!(entries[0].text, "padded");
    assert_eq!(entries[1].text, "/plan off <tag> **bold**");
}

/// TC-PORT-FEED-3: an empty remark is refused and records nothing.
///
/// Upstream: "rejects empty and whitespace-only input as a failed command
/// record".
///
/// Input: an empty string and a whitespace-only one, through the domain and
/// through the tool.
/// Expected: refused each time, with nothing on the journal. An empty remark
/// records nothing, and a record of nothing is worse than no record: a person
/// reading the feedback would have to work out that it says nothing.
#[tokio::test]
async fn an_empty_remark_is_refused_and_writes_nothing() {
    let h = Fixture::new("empty").await;
    h.register(FeedbackTool::new(h.log()));

    let direct = feedback::record(h.log().as_ref(), "   \n ", None);
    let through_tool = h.call(FeedbackTool::NAME, json!({ "text": "" })).await;

    assert!(direct.is_err());
    assert!(!through_tool.ok);
    assert!(
        through_tool.content.contains("needs something to say"),
        "{}",
        through_tool.content
    );
    assert!(h.events(feedback::topic::FEEDBACK_RECORDED).is_empty());
}

/// TC-PORT-FEED-4: feedback never reaches the model.
///
/// Upstream: "keeps every recorded event out of model context and derived
/// history".
///
/// Input: a journal with a user message, an assistant message and a remark,
/// derived into model messages.
/// Expected: the remark is in neither the derived history nor any message's
/// content. A person writing "this is going badly" is talking to the operator,
/// not steering the model, and a model reading its own operator feedback would
/// be reasoning about a channel it is not a party to.
#[tokio::test]
async fn a_remark_is_not_part_of_the_conversation() {
    let h = Fixture::new("invisible").await;
    h.register(FeedbackTool::new(h.log()));
    h.append(
        tetanus_turn::log::topic::USER_MESSAGE,
        json!({ "content": "do the thing" }),
    );
    h.call(
        FeedbackTool::NAME,
        json!({ "text": "the instructions contradict each other" }),
    )
    .await;

    let history = tetanus_turn::log::derive_messages(&h.log().events());

    assert_eq!(history.len(), 1, "only the user message derives");
    assert!(!history
        .iter()
        .any(|message| message.content.contains("contradict")));
    assert_eq!(
        recorded(&h.log().events()).len(),
        1,
        "it was still recorded"
    );
}

/// TC-PORT-FEED-5: the tool records what the model said and says it landed.
///
/// Upstream: "acknowledges feedback and records its payload exactly once in the
/// domain event".
///
/// Input: one call.
/// Expected: exactly one record carrying the text and naming the model as its
/// author, and an acknowledgement that tells the model to carry on - a tool
/// that left the model waiting for a reply would turn a report into a
/// conversation.
#[tokio::test]
async fn the_tool_records_once_and_acknowledges() {
    let h = Fixture::new("tool").await;
    h.register(FeedbackTool::new(h.log()));

    let outcome = h
        .call(
            FeedbackTool::NAME,
            json!({ "text": "there is no test runner configured" }),
        )
        .await;

    assert!(outcome.ok);
    assert!(
        outcome.content.contains("continue with the work"),
        "{}",
        outcome.content
    );
    let entries = recorded(&h.log().events());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].text, "there is no test runner configured");
    assert_eq!(entries[0].author.as_deref(), Some("model"));
}

/// TC-PORT-FEED-6: the remarks survive a reload.
///
/// Upstream: the records are durable session events.
///
/// Input: two remarks, then a replay from disk.
/// Expected: both, in order, from the cold journal.
#[tokio::test]
async fn the_remarks_survive_a_reload() {
    let h = Fixture::new("feedback-reload").await;
    feedback::record(h.log().as_ref(), "first", Some("operator")).expect("recorded");
    feedback::record(h.log().as_ref(), "second", None).expect("recorded");
    h.flush();

    let entries = recorded(&h.replay());

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].author.as_deref(), Some("operator"));
    assert_eq!(entries[1].author, None, "unattributed is not anonymous");
}
