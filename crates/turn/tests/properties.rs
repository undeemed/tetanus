//! Test Design Specification: the session log's derivation invariants, as
//! properties.
//!
//! Feature under test: what a journal of arbitrary events guarantees to the
//! turn engine that reads it back - `JsonlSessionLog`, `replay` and
//! `derive_messages`. Upstream pins the same invariants with fast-check in
//! `packages/core/session/tests/properties.spec.ts`; each case names the
//! upstream property it comes from.
//!
//! Approach: generate logs of up to twelve events, message-producing and not,
//! append them to a real journal in a temp directory, and assert the
//! invariant over every generated log rather than over one fixture. Text is
//! arbitrary Rust `String`s, so quotes, newlines and astral-plane characters
//! are generated, not special-cased.
//!
//! Features NOT tested here: what any one event means to a model
//! (`upstream_history.rs`), what a torn journal does (`upstream_repair.rs`),
//! and what the log broadcasts (`upstream_session.rs`). Those are example
//! suites; this file only pins what must hold for every log.
//!
//! Environmental needs: a writable temp directory. No case reaches a network
//! or an API key. Thirty-two cases per property, so a failure names a
//! shrunken counterexample in about a second.
//!
//! Pass criteria: each case's stated expected result holds for every
//! generated log.
//! Fail criteria: any counterexample, or a panic.

use proptest::prelude::*;
use tempfile::TempDir;
use tetanus_core::EventBus;
use tetanus_session::{replay, JsonlSessionLog, SessionEvent, SessionLog};
use tetanus_turn::llm::Role;
use tetanus_turn::log::{derive_messages, topic};

/// One event as a case asks for it, before the log gives it a seq and a time.
#[derive(Debug, Clone)]
struct Appended {
    ty: &'static str,
    data: serde_json::Value,
    sources: Option<Vec<u64>>,
}

/// An event that becomes a message the model reads.
fn message_event() -> impl Strategy<Value = Appended> {
    prop_oneof![
        any::<String>().prop_map(|content| Appended {
            ty: topic::USER_MESSAGE,
            data: serde_json::json!({ "content": content }),
            sources: Some(vec![]),
        }),
        (any::<String>(), any::<String>()).prop_map(|(content, name)| Appended {
            ty: topic::ASSISTANT_MESSAGE,
            data: serde_json::json!({
                "content": content,
                "tool_calls": [{ "id": "call-1", "name": name, "arguments": {} }],
            }),
            sources: Some(vec![]),
        }),
        (any::<String>(), any::<String>()).prop_map(|(id, content)| Appended {
            ty: topic::TOOL_RESULT,
            data: serde_json::json!({ "call_id": id, "content": content }),
            sources: Some(vec![]),
        }),
    ]
}

/// An event the model never reads: trace and replay data, plus the
/// assistant message that says nothing and calls nothing, which the
/// derivation drops on purpose.
fn silent_event() -> impl Strategy<Value = Appended> {
    prop_oneof![
        Just(Appended {
            ty: topic::TURN_START,
            data: serde_json::json!({ "turn": 1 }),
            sources: None,
        }),
        Just(Appended {
            ty: topic::STEP_START,
            data: serde_json::json!({ "turn": 1, "step": 1 }),
            sources: None,
        }),
        Just(Appended {
            ty: topic::STEP_END,
            data: serde_json::json!({ "turn": 1, "step": 1 }),
            sources: None,
        }),
        Just(Appended {
            ty: topic::TURN_END,
            data: serde_json::json!({ "turn": 1, "reason": "completed" }),
            sources: None,
        }),
        any::<String>().prop_map(|text| Appended {
            ty: topic::ASSISTANT_CHUNK,
            data: serde_json::json!({ "turn": 1, "step": 1, "text": text }),
            sources: None,
        }),
        Just(Appended {
            ty: topic::ASSISTANT_MESSAGE,
            data: serde_json::json!({ "content": "", "tool_calls": [] }),
            sources: Some(vec![]),
        }),
    ]
}

fn any_log() -> impl Strategy<Value = Vec<Appended>> {
    prop::collection::vec(prop_oneof![message_event(), silent_event()], 0..12)
}

/// Append a generated log to a real journal, and hand back what it holds.
fn journal(events: &[Appended]) -> (TempDir, std::path::PathBuf, Vec<SessionEvent>) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("journal.jsonl");
    let log = JsonlSessionLog::create("prop", &path, EventBus::new()).expect("journal");
    for event in events {
        match &event.sources {
            Some(sources) => log
                .append_with_sources(event.ty, event.data.clone(), sources.clone())
                .expect("append"),
            None => log.append(event.ty, event.data.clone()).expect("append"),
        };
    }
    let held = log.events();
    (dir, path, held)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// TC-PROP-SESS-1: a journal numbers its events from zero, one at a time.
    ///
    /// Upstream: "seq is strictly monotonic and zero-based contiguous".
    ///
    /// Expected: for any log, the event at position `i` has `seq == i`, and
    /// the journal holds exactly as many events as were appended. This is
    /// what lets `sourceEventSeqs` cite a position and `replay` verify one.
    #[test]
    fn seq_counts_the_log_from_zero(events in any_log()) {
        let (_dir, _path, held) = journal(&events);

        prop_assert_eq!(held.len(), events.len());
        for (i, event) in held.iter().enumerate() {
            prop_assert_eq!(event.seq, i as u64);
        }
    }

    /// TC-PROP-SESS-2: a journal read back from disk is the journal that was
    /// written, and derives the same history.
    ///
    /// Upstream: "replay-from-seed reproduces the derivation identically".
    ///
    /// Expected: `replay` of the file equals the events the log held in
    /// memory, event for event, and both derive the same messages. Arbitrary
    /// text makes this the JSONL round-trip too: a quote, a newline or an
    /// astral-plane character inside a message must not become a second line
    /// or a lost one.
    #[test]
    fn a_journal_read_back_is_the_journal_written(events in any_log()) {
        let (_dir, path, held) = journal(&events);

        let replayed = replay(&path).expect("a journal this process just wrote");

        prop_assert_eq!(&replayed, &held);
        prop_assert_eq!(derive_messages(&replayed), derive_messages(&held));
    }

    /// TC-PROP-SESS-3: events the model never reads change nothing about what
    /// it does read, wherever they fall.
    ///
    /// Upstream: "non-message events never affect derived history (any
    /// interleaving)".
    ///
    /// Expected: the history derived from the messages alone equals the
    /// history derived from those same messages with trace events shuffled
    /// through them, for any interleaving that keeps the messages in order.
    #[test]
    fn silent_events_never_reach_the_model(
        messages in prop::collection::vec(message_event(), 0..8),
        noise in prop::collection::vec(silent_event(), 0..8),
        picks in prop::collection::vec(any::<bool>(), 16),
    ) {
        let (_clean_dir, _clean_path, clean) = journal(&messages);

        let mut interleaved = Vec::new();
        let (mut m, mut n) = (0, 0);
        while m < messages.len() || n < noise.len() {
            let take_noise = n < noise.len()
                && (m >= messages.len() || picks[(m + n) % picks.len()]);
            if take_noise {
                interleaved.push(noise[n].clone());
                n += 1;
            } else {
                interleaved.push(messages[m].clone());
                m += 1;
            }
        }
        let (_dir, _path, mixed) = journal(&interleaved);

        prop_assert_eq!(derive_messages(&mixed), derive_messages(&clean));
    }

    /// TC-PROP-SESS-4: every derived message is one the wire knows how to
    /// carry.
    ///
    /// Upstream: "every derived message has a known role and is frozen
    /// (append-only contract)". Rust's derivation hands back an owned `Vec`,
    /// so there is nothing to freeze; what is left to pin is the role, the
    /// tool-call identity every tool message needs, and that reading the log
    /// twice does not change it.
    ///
    /// Expected: no derived message is a system message, every `tool` message
    /// carries the call id it answers, and the log is untouched by the
    /// derivation.
    #[test]
    fn every_derived_message_is_one_the_wire_can_carry(events in any_log()) {
        let (_dir, _path, held) = journal(&events);

        let derived = derive_messages(&held);

        for message in &derived {
            prop_assert_ne!(message.role, Role::System);
            if message.role == Role::Tool {
                prop_assert!(message.tool_call_id.is_some());
            }
        }
        prop_assert_eq!(derive_messages(&held), derived);
    }

    /// TC-PROP-SESS-5: an assistant turn that said nothing and called nothing
    /// stays off the model's history, however many of them there are.
    ///
    /// Upstream keeps this as an example; as a property it says the count,
    /// not one case: the derivation is exactly as long as the events that
    /// produce a message.
    ///
    /// Expected: the derived history has one message per `user/message`, per
    /// `tool/result`, and per `assistant/message` that carries content or a
    /// tool call - and nothing else, whatever else the log holds.
    #[test]
    fn a_silent_assistant_message_derives_to_nothing(events in any_log()) {
        let (_dir, _path, held) = journal(&events);

        let expected = held
            .iter()
            .filter(|event| match event.ty.as_str() {
                topic::USER_MESSAGE | topic::TOOL_RESULT => true,
                topic::ASSISTANT_MESSAGE => {
                    let content = event.data.get("content").and_then(|v| v.as_str());
                    let calls = event.data.get("tool_calls").and_then(|v| v.as_array());
                    content.is_some_and(|text| !text.is_empty())
                        || calls.is_some_and(|calls| !calls.is_empty())
                }
                _ => false,
            })
            .count();

        prop_assert_eq!(derive_messages(&held).len(), expected);
    }
}
