//! Test Design Specification: permission presets, ported.
//!
//! Feature under test: `tetanus_fs::preset` - the two independent knobs named
//! as one choice, the durable record of that choice, and the fold that tells a
//! resumed session what it is under. Upstream pins the same decisions in
//! `packages/interaction/permission-presets/tests/permission-presets.spec.ts`
//! and its `projection.spec.ts`.
//!
//! Approach: a real journal, because the whole mechanism is what is written and
//! what folds back out of it. Asserting on an in-memory struct would test a
//! cache of the thing under test.
//!
//! What is not restated, and why. Upstream ships the read side as a session
//! projection and the write side as a `/permission` slash command; both are
//! surfaces, and `docs/interface-contract.md` §5 puts a type the presentation
//! lane constructs on the other side of this boundary - so what is asserted
//! here is the fold and the switch, which is what either surface would call.
//! Its settings-schema half needs the per-namespace schemas
//! `docs/parity.md` still lists as a gap. Its `custom` pseudo-preset has a
//! counterpart in [`effective_preset`] answering `None` when nothing was
//! chosen, rather than a reserved name a table must not use.
//!
//! Environmental needs: a writable temporary directory.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

mod support;

use std::sync::Arc;

use support::Fixture;
use tetanus_core::EventBus;
use tetanus_fs::access::FsMode;
use tetanus_fs::preset::{
    effective_mode, effective_preset, preset, switch, topic, DEFAULT_PRESET, PRESETS,
};
use tetanus_session::{JsonlSessionLog, SessionLog};
use tetanus_turn::approval::{effective_policy, ApprovalPolicy};
use tetanus_turn::log::topic as turn_topic;

fn journal(fixture: &Fixture, name: &str) -> Arc<dyn SessionLog> {
    JsonlSessionLog::create(
        name,
        fixture.root().join(format!("{name}.jsonl")),
        EventBus::new(),
    )
    .expect("journal")
}

fn types(log: &Arc<dyn SessionLog>) -> Vec<String> {
    log.events().iter().map(|event| event.ty.clone()).collect()
}

/// TC-PORT-INT-12: a switch records the choice, then the knobs it changed.
///
/// Upstream: "a switch records the selected preset, then writes changed knobs
/// through their canonical setters".
///
/// Input: a fresh journal switched to `read-only`.
/// Expected: `permission/preset` first, then `fs/mode` and `approval/policy`;
/// and both folds report the preset's values. The intent comes first so a
/// reader sees the choice before its consequences, and the knobs are separate
/// events because every reader - the fence, the gate - keeps reading its own.
#[test]
fn a_switch_records_the_choice_and_then_the_knobs() {
    let fixture = Fixture::new();
    let log = journal(&fixture, "switch");

    let applied = switch(log.as_ref(), "read-only").expect("switch");

    assert_eq!(applied.name, "read-only");
    assert_eq!(
        types(&log),
        [
            topic::PERMISSION_PRESET,
            topic::FS_MODE,
            turn_topic::APPROVAL_POLICY
        ]
    );
    let events = log.events();
    assert_eq!(effective_preset(&events).as_deref(), Some("read-only"));
    assert_eq!(effective_mode(&events), Some(FsMode::ReadOnly));
    assert_eq!(effective_policy(&events), Some(ApprovalPolicy::Never));
}

/// TC-PORT-INT-13: a knob already at the value it needs is not rewritten.
///
/// Upstream: the switch writes knobs "through their canonical setters", which
/// are idempotent.
///
/// Input: `read-only`, then `danger-full-access`, which shares neither knob,
/// then `read-only` again.
/// Expected: the second switch writes both knobs; each switch writes its intent
/// even when nothing else changes, because the choice is what a person made and
/// a surface shows it back to them; and the folds follow the last switch.
#[test]
fn switching_writes_the_intent_every_time_and_only_the_knobs_that_moved() {
    let fixture = Fixture::new();
    let log = journal(&fixture, "idempotent");

    switch(log.as_ref(), "read-only").expect("first");
    let after_first = log.events().len();
    switch(log.as_ref(), "read-only").expect("again");
    let after_repeat = log.events().len();
    switch(log.as_ref(), "danger-full-access").expect("widen");

    assert_eq!(
        after_repeat - after_first,
        1,
        "the same preset again is one intent event and no knob writes"
    );
    let events = log.events();
    assert_eq!(
        effective_preset(&events).as_deref(),
        Some("danger-full-access")
    );
    assert_eq!(effective_mode(&events), Some(FsMode::DangerFullAccess));
    assert_eq!(effective_policy(&events), Some(ApprovalPolicy::Ask));
}

/// TC-PORT-INT-14: a preset nobody defined is refused, and writes nothing.
///
/// Upstream: the table is the closed list of switch targets.
///
/// Input: a switch to a name that is not in the table.
/// Expected: refused, naming the presets that do exist, and not one event
/// appended. A journal carrying a preset nothing can read back would leave a
/// resumed session under knobs no fold could explain.
#[test]
fn an_unknown_preset_is_refused_and_leaves_the_journal_untouched() {
    let fixture = Fixture::new();
    let log = journal(&fixture, "unknown");

    let refused = switch(log.as_ref(), "yolo").expect_err("refused");

    assert!(refused.to_string().contains("yolo"), "{refused}");
    assert!(
        refused.to_string().contains("workspace-write"),
        "it says what the presets are: {refused}"
    );
    assert!(log.events().is_empty());
}

/// TC-PORT-INT-15: the table, and where tetanus parts from upstream.
///
/// Upstream: two presets, with `danger-full-access` bundling the `never`
/// approval policy.
///
/// Input: the shipped table.
/// Expected: three presets in widening order; the default is
/// `workspace-write`; and `danger-full-access` bundles `ask`, not `never`. The
/// last is the deliberate divergence: in tetanus `never` settles every ask
/// `rejected` (contract §4.4.7), so bundling it with full access would make the
/// widest preset refuse the very calls the narrower one allows.
#[test]
fn the_table_widens_and_the_widest_preset_still_asks() {
    let names: Vec<&str> = PRESETS.iter().map(|preset| preset.name).collect();

    assert_eq!(
        names,
        ["read-only", "workspace-write", "danger-full-access"]
    );
    assert_eq!(DEFAULT_PRESET, "workspace-write");
    let read_only = preset("read-only").expect("in the table");
    let workspace = preset("workspace-write").expect("in the table");
    let full = preset("danger-full-access").expect("in the table");
    assert_eq!(
        (read_only.mode, read_only.approval),
        (FsMode::ReadOnly, ApprovalPolicy::Never)
    );
    assert_eq!(
        (workspace.mode, workspace.approval),
        (FsMode::WorkspaceWrite, ApprovalPolicy::Ask)
    );
    assert_eq!(
        (full.mode, full.approval),
        (FsMode::DangerFullAccess, ApprovalPolicy::Ask),
        "the widest preset still puts an irreversible call to somebody"
    );
    for entry in PRESETS {
        assert!(
            entry.description.ends_with('.'),
            "{} reads as a sentence a person can choose by",
            entry.name
        );
    }
}

/// TC-PORT-INT-16: the preset a journal holds is what the backend and the gate
/// are composed from.
///
/// Upstream: the knobs are what execute; the preset is the intent over them.
///
/// Input: a journal switched to `read-only`, folded back into a backend and a
/// policy.
/// Expected: the backend refuses a write, and the policy is the one the gate
/// would apply. This is the case that would still pass if the switch wrote
/// nothing and the table were consulted directly - so it asserts the round
/// trip through the journal, which is what a resumed session actually does.
#[test]
fn a_resumed_session_composes_its_backend_from_what_the_journal_holds() {
    let fixture = Fixture::new();
    fixture.write("kept.txt", "original\n");
    let log = journal(&fixture, "resumed");
    switch(log.as_ref(), "read-only").expect("switch");

    let events = log.events();
    let mode = effective_mode(&events).expect("a mode was written");
    let policy = effective_policy(&events).expect("a policy was written");
    let backend = tetanus_fs::backend(mode, fixture.root()).expect("backend");
    let target = backend.resolve("kept.txt").expect("resolve");
    let refused = backend
        .write(
            &target,
            "changed\n",
            &tetanus_fs::service::WriteIntent::Unconditional,
        )
        .expect_err("read-only refuses");

    assert_eq!(policy, ApprovalPolicy::Never);
    assert!(refused.to_string().contains("read-only mode"), "{refused}");
    assert_eq!(fixture.read("kept.txt"), "original\n");
}
