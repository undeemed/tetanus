//! Conformance: folding several hooks' answers at one point into one.
//!
//! Feature under test: `tetanus_hooks::merge::merge_hook_outputs` — the
//! most-restrictive fold that decides what a hook point resolved to.
//!
//! Ported from upstream `packages/hooks/hook-protocol/tests/merge.spec.ts`.
//! Case ids TC-HOOK-MERGE-1..12. The last two are this port's own.

use tetanus_hooks::merge_hook_outputs;
use tetanus_hooks::types::{HookDecision, HookOutput, MergedDecision};

use HookDecision::{Allow, Approve, Ask, Block, Deny};
use MergedDecision as Merged;

/// A hook that answered with a decision and nothing else.
fn decided(decision: HookDecision) -> HookOutput {
    HookOutput {
        decision: Some(decision),
        ..HookOutput::default()
    }
}

/// A hook that answered with a decision and explained it.
fn because(decision: HookDecision, reason: &str) -> HookOutput {
    HookOutput {
        decision: Some(decision),
        reason: Some(reason.to_owned()),
        ..HookOutput::default()
    }
}

/// TC-HOOK-MERGE-1: no hooks is not an answer.
#[test]
fn an_empty_list_folds_to_the_neutral_outcome() {
    let merged = merge_hook_outputs(&[]);
    assert_eq!(merged.decision, Merged::None);
    assert!(!merged.stop);
    assert!(merged.additional_context.is_empty());
    assert!(merged.system_messages.is_empty());
    assert_eq!(merged.reason, None);
}

/// TC-HOOK-MERGE-2: both permitting spellings mean the same thing.
#[test]
fn approve_and_allow_are_one_answer() {
    assert_eq!(
        merge_hook_outputs(&[decided(Allow)]).decision,
        Merged::Allow
    );
    assert_eq!(
        merge_hook_outputs(&[decided(Approve)]).decision,
        Merged::Allow
    );
}

/// TC-HOOK-MERGE-3: the most restrictive answer wins, whatever order it came
/// in, and `block` folds to the same place as `deny`.
#[test]
fn the_most_restrictive_answer_wins_in_any_order() {
    let cases: [(&[HookDecision], Merged); 4] = [
        (&[Allow, Ask], Merged::Ask),
        (&[Ask, Deny], Merged::Deny),
        (&[Deny, Allow], Merged::Deny),
        (&[Allow, Block], Merged::Deny),
    ];
    for (decisions, expected) in cases {
        let outputs: Vec<HookOutput> = decisions.iter().copied().map(decided).collect();
        assert_eq!(
            merge_hook_outputs(&outputs).decision,
            expected,
            "{decisions:?} should fold to {expected:?}"
        );
    }
}

/// TC-HOOK-MERGE-4: hooks that expressed no permission answer resolve to none.
#[test]
fn hooks_that_said_nothing_resolve_to_none() {
    let quiet = [HookOutput::default(), HookOutput::default()];
    assert_eq!(merge_hook_outputs(&quiet).decision, Merged::None);
}

/// TC-HOOK-MERGE-5: several objections to the same answer are joined by a
/// blank line, and a permitting hook's reason is not among them.
#[test]
fn objections_join_with_a_blank_line_and_an_allow_reason_is_not_one() {
    let merged = merge_hook_outputs(&[
        because(Deny, "first objection"),
        because(Allow, "this allow reason is not collected"),
        because(Block, "second objection"),
    ]);
    assert_eq!(
        merged.reason.as_deref(),
        Some("first objection\n\nsecond objection")
    );
}

/// TC-HOOK-MERGE-6: nothing objected, so there is nothing to explain.
#[test]
fn a_permitted_outcome_carries_no_reason() {
    assert_eq!(merge_hook_outputs(&[because(Allow, "why")]).reason, None);
}

/// TC-HOOK-MERGE-7: the reason surfaced belongs to the answer that won.
#[test]
fn an_ask_that_wins_shows_the_ask_reason() {
    let merged = merge_hook_outputs(&[
        because(Allow, "allow reason, not surfaced"),
        because(Ask, "needs approval"),
    ]);
    assert_eq!(merged.decision, Merged::Ask);
    assert_eq!(merged.reason.as_deref(), Some("needs approval"));
}

/// TC-HOOK-MERGE-8: when a denial outranks an ask, the ask's reason goes with
/// it. Explaining a refusal with the text of a question would misdescribe it.
#[test]
fn a_denial_that_outranks_an_ask_drops_the_ask_reason() {
    let merged = merge_hook_outputs(&[
        because(Ask, "ask reason, not surfaced once deny wins"),
        because(Deny, "the real objection"),
    ]);
    assert_eq!(merged.decision, Merged::Deny);
    assert_eq!(merged.reason.as_deref(), Some("the real objection"));
}

/// TC-HOOK-MERGE-9: a halt sticks, and keeps the first halting hook's reason.
#[test]
fn the_first_halt_wins_and_later_ones_do_not_overwrite_it() {
    let merged = merge_hook_outputs(&[
        HookOutput {
            proceed: Some(true),
            ..HookOutput::default()
        },
        HookOutput {
            proceed: Some(false),
            stop_reason: Some("halt now".into()),
            ..HookOutput::default()
        },
        HookOutput {
            proceed: Some(false),
            stop_reason: Some("second halt, ignored".into()),
            ..HookOutput::default()
        },
    ]);
    assert!(merged.stop);
    assert_eq!(merged.stop_reason.as_deref(), Some("halt now"));
}

/// TC-HOOK-MERGE-10: every hook let the turn go on.
#[test]
fn no_halt_when_every_hook_proceeds() {
    let merged = merge_hook_outputs(&[
        HookOutput {
            proceed: Some(true),
            ..HookOutput::default()
        },
        HookOutput::default(),
    ]);
    assert!(!merged.stop);
    assert_eq!(merged.stop_reason, None);
}

/// TC-HOOK-MERGE-11: a halt with no reason still halts. The stop and its
/// explanation are separate facts, and a missing explanation must not read as
/// "did not stop".
#[test]
fn a_halt_without_a_reason_still_halts() {
    let merged = merge_hook_outputs(&[HookOutput {
        proceed: Some(false),
        ..HookOutput::default()
    }]);
    assert!(merged.stop);
    assert_eq!(merged.stop_reason, None);
}

/// TC-HOOK-MERGE-12: context and warnings keep hook order, and an empty one
/// is not a contribution.
#[test]
fn context_and_warnings_accumulate_in_hook_order_skipping_empties() {
    let merged = merge_hook_outputs(&[
        HookOutput {
            additional_context: Some("ctx-A".into()),
            system_message: Some("warn-A".into()),
            ..HookOutput::default()
        },
        HookOutput {
            additional_context: Some(String::new()),
            system_message: Some(String::new()),
            ..HookOutput::default()
        },
        HookOutput {
            additional_context: Some("ctx-B".into()),
            ..HookOutput::default()
        },
        HookOutput {
            system_message: Some("warn-B".into()),
            ..HookOutput::default()
        },
    ]);
    assert_eq!(merged.additional_context, ["ctx-A", "ctx-B"]);
    assert_eq!(merged.system_messages, ["warn-A", "warn-B"]);
}

/// TC-HOOK-MERGE-13: the fold does not depend on the order hooks answered in,
/// for the decision.
///
/// This port's own. Hooks run concurrently in later slices, so the order their
/// answers arrive in is not the order they were configured in. The decision
/// must be a function of the set; only the accumulating fields may be ordered.
#[test]
fn the_decision_does_not_depend_on_the_order_answers_arrive_in() {
    let every = [Approve, Allow, Block, Deny, Ask];
    for a in every {
        for b in every {
            let forwards = merge_hook_outputs(&[decided(a), decided(b)]).decision;
            let backwards = merge_hook_outputs(&[decided(b), decided(a)]).decision;
            assert_eq!(forwards, backwards, "{a:?} then {b:?} vs the reverse");
        }
    }
}

/// TC-HOOK-MERGE-14: a hook that only observes cannot change the answer.
///
/// This port's own, and the property a deployment relies on when it adds a
/// logging hook to a point that already has a gate: adding a silent hook
/// anywhere in the list must leave the decision, the reason and the halt
/// exactly as they were.
#[test]
fn adding_a_silent_hook_changes_nothing_but_where_it_sits() {
    let busy = [
        because(Ask, "please confirm"),
        HookOutput {
            proceed: Some(false),
            stop_reason: Some("halt".into()),
            additional_context: Some("ctx".into()),
            ..HookOutput::default()
        },
    ];
    let expected = merge_hook_outputs(&busy);

    for position in 0..=busy.len() {
        let mut with_observer = busy.to_vec();
        with_observer.insert(position, HookOutput::default());
        let actual = merge_hook_outputs(&with_observer);
        assert_eq!(
            actual, expected,
            "a silent hook at {position} changed the fold"
        );
    }
}
