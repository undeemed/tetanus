//! Conformance: how deep delegation may go.
//!
//! Feature under test: `tetanus_subagent::depth` — the recursion budget a
//! parent passes to its children, and the monotone rule that keeps a resume
//! from buying back depth.
//!
//! Ported from upstream `packages/subagent/subagent/src/depth.ts` and the
//! depth rules of its `service.spec.ts` and `continuation.spec.ts`.
//! Case ids TC-SUB-DEPTH-1..10. The last three are this port's own.

use serde_json::json;
use tetanus_subagent::depth::{
    check_within_max, child_depth, delegation_depth_of, depth_from_json, DepthError,
};

/// TC-SUB-DEPTH-1: an agent nobody delegated to is at the top.
#[test]
fn an_agent_with_no_depth_anywhere_is_top_level() {
    assert_eq!(delegation_depth_of(None, None), 0);
}

/// TC-SUB-DEPTH-2: either source alone is believed.
#[test]
fn either_source_alone_gives_the_depth() {
    assert_eq!(delegation_depth_of(Some(2), None), 2);
    assert_eq!(delegation_depth_of(None, Some(3)), 3);
}

/// TC-SUB-DEPTH-3: runtime may deepen the count.
#[test]
fn runtime_may_deepen_the_count() {
    assert_eq!(delegation_depth_of(Some(1), Some(4)), 4);
}

/// TC-SUB-DEPTH-4: runtime may **not** shorten it.
///
/// The rule the whole module exists for. A resumed child arrives with fresh
/// options; if the runtime value were believed outright, a child resumed with
/// nothing set would count itself as top-level and delegate on a full budget.
#[test]
fn runtime_can_never_shorten_the_persisted_depth() {
    assert_eq!(
        delegation_depth_of(Some(5), Some(0)),
        5,
        "a resume must not buy back depth"
    );
    assert_eq!(delegation_depth_of(Some(5), None), 5);
}

/// TC-SUB-DEPTH-5: a child is one deeper than its parent.
#[test]
fn a_child_is_one_deeper_than_its_parent() {
    assert_eq!(child_depth(0), 1);
    assert_eq!(child_depth(7), 8);
}

/// TC-SUB-DEPTH-6: a cap refuses a child past it, and says both numbers.
#[test]
fn a_cap_refuses_a_child_past_it() {
    assert_eq!(
        check_within_max(1, Some(0)),
        Err(DepthError::TooDeep {
            attempted: 1,
            max: 0
        })
    );
    assert_eq!(
        check_within_max(1, Some(0)).unwrap_err().to_string(),
        "subagent depth 1 exceeds maxDepth 0"
    );
}

/// TC-SUB-DEPTH-7: a child at exactly the cap is allowed, and no cap is no
/// limit.
#[test]
fn the_cap_is_inclusive_and_optional() {
    assert_eq!(check_within_max(3, Some(3)), Ok(()));
    assert_eq!(check_within_max(u64::MAX, None), Ok(()));
}

/// TC-SUB-DEPTH-8: a written-down depth that is not a whole count is refused,
/// naming the field.
#[test]
fn a_depth_that_is_not_a_whole_count_is_refused() {
    for bad in [json!(-1), json!(1.5), json!("2"), json!(true), json!([])] {
        assert_eq!(
            depth_from_json(Some(&bad), "maxDepth"),
            Err(DepthError::NotAWholeCount { field: "maxDepth" }),
            "for {bad}"
        );
    }
    assert_eq!(
        depth_from_json(Some(&json!(-1)), "maxDepth")
            .unwrap_err()
            .to_string(),
        "maxDepth must be a non-negative safe integer"
    );
}

/// TC-SUB-DEPTH-9: an unset cap is not a cap of zero.
///
/// This port's own, and the distinction is load-bearing in the opposite
/// direction from most absent-field rules: zero *forbids* delegation, so
/// reading "unset" as zero would silently disable every subagent, while
/// reading it as unlimited is what "I did not configure a cap" means.
#[test]
fn an_unset_cap_is_not_a_cap_of_zero() {
    assert_eq!(depth_from_json(None, "maxDepth"), Ok(None));
    assert_eq!(depth_from_json(Some(&json!(null)), "maxDepth"), Ok(None));
    assert_eq!(check_within_max(9, None), Ok(()), "unset means unlimited");
    assert!(
        check_within_max(1, Some(0)).is_err(),
        "zero means no delegation at all"
    );
}

/// TC-SUB-DEPTH-10: a whole number written as a float is not refused for a
/// spelling.
///
/// This port's own. JSON does not distinguish `2` from `2.0`, and a settings
/// file written by a tool that emits floats would otherwise have every depth
/// refused. `2.5` is still refused, because that one is a mistake rather than
/// a spelling.
#[test]
fn a_whole_number_written_as_a_float_is_accepted() {
    assert_eq!(depth_from_json(Some(&json!(2.0)), "maxDepth"), Ok(Some(2)));
    assert_eq!(depth_from_json(Some(&json!(0.0)), "maxDepth"), Ok(Some(0)));
    assert_eq!(
        depth_from_json(Some(&json!(2.5)), "maxDepth"),
        Err(DepthError::NotAWholeCount { field: "maxDepth" })
    );
}

/// TC-SUB-DEPTH-11: the depth count cannot wrap.
///
/// This port's own. `child_depth` increments a number that came from a
/// persisted header, and a wrap would turn the deepest possible child into a
/// top-level agent with a full delegation budget - which is the exact failure
/// the monotone rule exists to prevent, reached by arithmetic instead.
#[test]
fn the_depth_count_saturates_rather_than_wrapping() {
    assert_eq!(child_depth(u64::MAX), u64::MAX);
    assert!(
        check_within_max(child_depth(u64::MAX), Some(10)).is_err(),
        "a saturated depth is still past any real cap"
    );
}
