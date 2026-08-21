//! Conformance: the durable record that says what a child is.
//!
//! Feature under test: `tetanus_subagent::descriptor` — the versioned
//! `subagent/descriptor` record, and what reading it back refuses.
//!
//! Ported from upstream `packages/subagent/subagent/src/descriptor.ts`, whose
//! rules its `continuation.spec.ts` and `list-children.spec.ts` exercise
//! through the service. Case ids TC-SUB-DESC-1..12; the last four are this
//! port's own.

use serde_json::json;
use tetanus_session::SessionEvent;
use tetanus_subagent::descriptor::{
    descriptor_payload, fold_descriptor, parse_descriptor, SubagentDescriptor, SubagentMode,
    ToolFilter, DESCRIPTOR_EVENT, SUBAGENT_DESCRIPTOR_VERSION,
};

fn event(ty: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        ty: ty.to_owned(),
        seq: 0,
        time: 0,
        data,
        source_event_seqs: None,
    }
}

fn one_shot() -> serde_json::Value {
    json!({"version": SUBAGENT_DESCRIPTOR_VERSION, "mode": "one-shot", "provider": "spawn"})
}

fn continuable() -> serde_json::Value {
    json!({
        "version": SUBAGENT_DESCRIPTOR_VERSION,
        "mode": "continuable",
        "provider": "fork",
        "label": "reviewer",
    })
}

/// TC-SUB-DESC-1: a one-shot record reads back.
#[test]
fn a_one_shot_record_reads_back() {
    let parsed = parse_descriptor(&one_shot())
        .expect("readable")
        .expect("present");
    assert_eq!(parsed.mode, SubagentMode::OneShot);
    assert_eq!(parsed.provider, "spawn");
    assert_eq!(parsed.label, None);
}

/// TC-SUB-DESC-2: a continuable record carries its resumable composition.
#[test]
fn a_continuable_record_carries_its_composition() {
    let mut raw = continuable();
    raw["agentProvider"] = json!("deepseek");
    raw["agentModel"] = json!("deepseek-chat");
    raw["persona"] = json!("critic");
    raw["toolFilter"] = json!({"deny": ["bash"]});

    let parsed = parse_descriptor(&raw).expect("readable").expect("present");
    assert_eq!(parsed.mode, SubagentMode::Continuable);
    assert_eq!(parsed.label.as_deref(), Some("reviewer"));
    assert_eq!(parsed.agent_provider.as_deref(), Some("deepseek"));
    assert_eq!(parsed.agent_model.as_deref(), Some("deepseek-chat"));
    assert_eq!(parsed.persona.as_deref(), Some("critic"));
    assert_eq!(
        parsed.tool_filter,
        Some(ToolFilter {
            allow: None,
            deny: Some(vec!["bash".into()]),
        })
    );
}

/// TC-SUB-DESC-3: a version this build does not know reads as absent, not as
/// an error.
///
/// A newer runtime wrote this child. It cannot be classified here, and saying
/// so is different from saying the record is broken.
#[test]
fn an_unknown_version_reads_as_absent() {
    let mut future = one_shot();
    future["version"] = json!(SUBAGENT_DESCRIPTOR_VERSION + 1);
    assert_eq!(parse_descriptor(&future), Ok(None));

    let mut ancient = one_shot();
    ancient["version"] = json!(1);
    assert_eq!(parse_descriptor(&ancient), Ok(None));
}

/// TC-SUB-DESC-4: an undeclared field at *this* version is refused.
///
/// The other half of TC-SUB-DESC-3, and the reason they differ: a record at
/// the current version carrying a field this version does not declare is
/// corrupt or hand-edited, and reading it would silently ignore composition
/// somebody asked for.
#[test]
fn an_undeclared_field_at_this_version_is_refused() {
    let mut raw = one_shot();
    raw["persona"] = json!("not valid on a one-shot child");
    let error = parse_descriptor(&raw).expect_err("refused");
    assert_eq!(
        error.to_string(),
        "persisted subagent descriptor payload has unknown field \"persona\""
    );

    let mut invented = continuable();
    invented["somethingNew"] = json!(1);
    assert!(parse_descriptor(&invented).is_err());
}

/// TC-SUB-DESC-5: the required fields are required, and named when missing.
#[test]
fn the_required_fields_are_required() {
    for (raw, expected) in [
        (json!("not an object"), "payload must be an object"),
        (
            json!({"mode": "one-shot", "provider": "p"}),
            "version must be a number",
        ),
        (
            json!({"version": SUBAGENT_DESCRIPTOR_VERSION, "provider": "p"}),
            "mode must be \"one-shot\" or \"continuable\"",
        ),
        (
            json!({"version": SUBAGENT_DESCRIPTOR_VERSION, "mode": "one-shot"}),
            "provider must be a string",
        ),
    ] {
        let error = parse_descriptor(&raw).expect_err("refused");
        assert_eq!(
            error.to_string(),
            format!("persisted subagent descriptor {expected}")
        );
    }
}

/// TC-SUB-DESC-6: a continuable child must be nameable.
///
/// Enumeration has to identify the conversation without replaying the parent's
/// tool results or exposing the child's prompt, so the label is the one field
/// a resumable child cannot omit.
#[test]
fn a_continuable_child_must_have_a_label() {
    let mut nameless = continuable();
    nameless.as_object_mut().expect("object").remove("label");
    let error = parse_descriptor(&nameless).expect_err("refused");
    assert_eq!(
        error.to_string(),
        "persisted subagent descriptor label must be a string"
    );

    // A one-shot child may be anonymous.
    assert!(parse_descriptor(&one_shot()).expect("readable").is_some());
}

/// TC-SUB-DESC-7: a wrong-typed field is refused, naming the field.
#[test]
fn a_wrong_typed_field_is_refused_by_name() {
    let mut raw = continuable();
    raw["persona"] = json!(7);
    assert_eq!(
        parse_descriptor(&raw).expect_err("refused").to_string(),
        "persisted subagent descriptor persona must be a string"
    );
}

/// TC-SUB-DESC-8: a tool filter that restricts nothing is a mistake.
///
/// An empty filter is indistinguishable from no filter in effect, so writing
/// one means something went wrong upstream of here — and reading it as
/// "unrestricted" would hand a child every tool when someone tried to limit it.
#[test]
fn a_tool_filter_that_restricts_nothing_is_refused() {
    let mut raw = continuable();
    raw["toolFilter"] = json!({});
    assert_eq!(
        parse_descriptor(&raw).expect_err("refused").to_string(),
        "persisted subagent descriptor toolFilter must declare allow and/or deny"
    );

    raw["toolFilter"] = json!({"allow": ["read"], "banned": ["x"]});
    assert_eq!(
        parse_descriptor(&raw).expect_err("refused").to_string(),
        "persisted subagent descriptor toolFilter has unknown field \"banned\""
    );

    raw["toolFilter"] = json!({"deny": ["ok", 7]});
    assert_eq!(
        parse_descriptor(&raw).expect_err("refused").to_string(),
        "persisted subagent descriptor toolFilter.deny must be an array of strings"
    );
}

/// TC-SUB-DESC-9: the fold finds the record in a journal, and reports its
/// absence as absence.
#[test]
fn the_fold_finds_the_record_or_says_there_is_none() {
    assert_eq!(fold_descriptor(&[]), Ok(None));
    assert_eq!(
        fold_descriptor(&[event("turn/start", json!({"turn": 1}))]),
        Ok(None)
    );

    let journal = [
        event("turn/start", json!({"turn": 1})),
        event(DESCRIPTOR_EVENT, one_shot()),
        event("turn/end", json!({"turn": 1})),
    ];
    let found = fold_descriptor(&journal)
        .expect("readable")
        .expect("present");
    assert_eq!(found.provider, "spawn");
}

/// TC-SUB-DESC-10: the first record wins.
///
/// This port's own. A child writes one descriptor in its first turn; a journal
/// holding two is a child re-seeded from another origin, and the establishing
/// record is the one that says what this child is. Scanning to the last would
/// let a later append rewrite a child's identity.
#[test]
fn the_first_record_is_the_one_that_counts() {
    let mut later = continuable();
    later["provider"] = json!("impostor");
    let journal = [
        event(DESCRIPTOR_EVENT, one_shot()),
        event(DESCRIPTOR_EVENT, later),
    ];
    let found = fold_descriptor(&journal)
        .expect("readable")
        .expect("present");
    assert_eq!(found.provider, "spawn");
    assert_eq!(found.mode, SubagentMode::OneShot);
}

/// TC-SUB-DESC-11: what is written reads back as what was written.
///
/// This port's own, and the property the whole module exists for: the record
/// crosses a process boundary and a resume that could not reconstruct the
/// composition would silently start a differently-composed child.
#[test]
fn a_written_record_reads_back_identically() {
    let original = SubagentDescriptor {
        mode: SubagentMode::Continuable,
        provider: "fork".into(),
        label: Some("reviewer".into()),
        agent_provider: Some("deepseek".into()),
        agent_model: Some("deepseek-chat".into()),
        persona: Some("critic".into()),
        tool_filter: Some(ToolFilter {
            allow: Some(vec!["read".into()]),
            deny: Some(vec!["bash".into()]),
        }),
    };
    let round_tripped = parse_descriptor(&descriptor_payload(&original))
        .expect("readable")
        .expect("present");
    assert_eq!(round_tripped, original);

    let minimal = SubagentDescriptor {
        mode: SubagentMode::OneShot,
        provider: "spawn".into(),
        label: None,
        agent_provider: None,
        agent_model: None,
        persona: None,
        tool_filter: None,
    };
    assert_eq!(
        parse_descriptor(&descriptor_payload(&minimal))
            .expect("readable")
            .expect("present"),
        minimal
    );
}

/// TC-SUB-DESC-12: an absent field is omitted from the payload, never null.
///
/// This port's own. A null would be an undeclared shape on the way back in —
/// `optional_string` refuses a non-string — so writing one would produce a
/// record this very module cannot read.
#[test]
fn an_absent_field_is_omitted_rather_than_written_null() {
    let anonymous = SubagentDescriptor {
        mode: SubagentMode::OneShot,
        provider: "spawn".into(),
        label: None,
        agent_provider: None,
        agent_model: None,
        persona: None,
        tool_filter: None,
    };
    let payload = descriptor_payload(&anonymous);
    assert_eq!(payload.get("label"), None, "omitted, not null");
    assert_eq!(
        payload,
        json!({"version": SUBAGENT_DESCRIPTOR_VERSION, "mode": "one-shot", "provider": "spawn"})
    );

    // A one-shot child never carries continuable composition, even if the
    // struct happens to hold some.
    let confused = SubagentDescriptor {
        persona: Some("ignored".into()),
        ..anonymous
    };
    assert_eq!(descriptor_payload(&confused).get("persona"), None);
}
