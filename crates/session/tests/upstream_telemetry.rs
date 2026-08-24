//! Test Design Specification: the capture side of session telemetry.
//!
//! Feature under test: `tetanus_session::telemetry` - which records exist,
//! what they carry, when they are taken, and the redaction seam a deployment
//! mounts rules on.
//!
//! Upstream: `packages/session/session-telemetry`, which owns exactly this
//! half and says so - everything downstream of `emit` is the reporting SDK's.
//! Its OpenTelemetry exporter is a separate package and stays a dependency
//! decision rather than a port, which is what lets the capture half land with
//! no dependency at all.
//!
//! Approach: a recording sink, a real journal, and rules that misbehave on
//! purpose. The two things worth proving are that a record cannot be mistaken
//! for the wrong kind of thing, and that nothing a deployment writes in a
//! redaction rule can hurt the turn that was merely writing to its journal.
//!
//! Features NOT tested here: batching, retry and export, which are not in this
//! layer by design.
//!
//! Environmental needs: a temporary directory. No network.
//!
//! Pass criteria: each case's stated expected result holds exactly.
//! Fail criteria: any other observed value, or a panic.

use std::sync::Arc;

use serde_json::json;
use tetanus_core::EventBus;
use tetanus_session::telemetry::{
    severity_of, Channel, RecordingSink, Severity, Telemetry, TelemetryRecord,
};
use tetanus_session::{JsonlSessionLog, SessionEvent, SessionLog};

fn journal(name: &str) -> (tempfile::TempDir, EventBus, Arc<dyn SessionLog>) {
    let home = tempfile::tempdir().expect("temp dir");
    let bus = EventBus::new();
    let log: Arc<dyn SessionLog> =
        JsonlSessionLog::create(name, home.path().join("t.jsonl"), bus.clone()).expect("journal");
    (home, bus, log)
}

fn event(ty: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        seq: 7,
        time: 1_700_000_000_000,
        ty: ty.into(),
        data,
        source_event_seqs: None,
    }
}

/// TC-PORT-TELEM-1: every append is captured, once, as a ledger record.
///
/// Telemetry reads the journal rather than writing a second one, so a record
/// exists exactly when an event does and no producer has to remember to
/// report. A capture path that had to be called explicitly would be a capture
/// path with holes in it wherever somebody forgot.
///
/// Capture also begins where it is attached and is not retroactive: the
/// `session/start` line is written by `create`, before there is a watch, so it
/// is not among the records. A deployment that wants the header exported
/// attaches before it opens the session, or reads the header off the journal -
/// inventing a record for an event nobody watched would put a time on the wire
/// that no listener ever saw.
///
/// Input: three appends on a watched bus, on a journal created before it.
/// Expected: exactly those three as ledger records in order, each carrying the
/// event's own seq, time, type and payload, and each naming the session.
#[test]
fn every_append_is_captured_once() {
    let (_home, bus, log) = journal("s-telem");
    let sink = Arc::new(RecordingSink::new());
    let telemetry = Telemetry::new("s-telem", Arc::clone(&sink) as Arc<_>);
    let _watch = telemetry.watch(&bus);

    log.append("turn/start", json!({ "turn": 1 }))
        .expect("appended");
    log.append("user/message", json!({ "content": "hi" }))
        .expect("appended");
    log.append("turn/end", json!({ "turn": 1, "stop_reason": "natural" }))
        .expect("appended");

    let records = sink.records();
    let ledger: Vec<&TelemetryRecord> = records
        .iter()
        .filter(|r| r.channel == Channel::Ledger)
        .collect();
    assert_eq!(
        ledger.iter().map(|r| r.ty.as_str()).collect::<Vec<_>>(),
        ["turn/start", "user/message", "turn/end"],
        "capture begins at the watch and is not retroactive"
    );
    let message = ledger
        .iter()
        .find(|r| r.ty == "user/message")
        .expect("captured");
    assert_eq!(message.body["content"], "hi");
    assert_eq!(message.session_id, "s-telem");
    assert!(
        message.seq.is_some(),
        "a ledger record carries its position"
    );
}

/// TC-PORT-TELEM-2: an operational record cannot be mistaken for a row of the
/// journal.
///
/// The two channels exist so a receiver counting exported rows against a
/// session's journal gets the same number. An ops record therefore carries no
/// `seq` at all rather than a plausible one.
///
/// Input: one journal append and one reported failure.
/// Expected: the first is `ledger` with a seq, the second is `ops` with none,
/// and both name the session.
#[test]
fn an_ops_record_carries_no_log_position() {
    let (_home, bus, log) = journal("s-ops");
    let sink = Arc::new(RecordingSink::new());
    let telemetry = Telemetry::new("s-ops", Arc::clone(&sink) as Arc<_>);
    let _watch = telemetry.watch(&bus);

    log.append("turn/start", json!({ "turn": 1 }))
        .expect("appended");
    telemetry.report(
        "agent-error",
        Severity::Error,
        json!({ "message": "the provider refused" }),
    );

    let records = sink.records();
    let ops = records
        .iter()
        .find(|r| r.channel == Channel::Ops)
        .expect("the failure was reported");
    assert_eq!(ops.seq, None, "an ops record has no position to claim");
    assert_eq!(ops.ty, "agent-error");
    assert_eq!(ops.severity, Severity::Error);
    assert_eq!(ops.session_id, "s-ops", "and is still attributable");
    assert!(
        records
            .iter()
            .filter(|r| r.channel == Channel::Ledger)
            .all(|r| r.seq.is_some()),
        "every ledger record has one"
    );
}

/// TC-PORT-TELEM-3: severity is read from the outcome, not from the type.
///
/// A receiver should be able to alert with no configuration, and the fact that
/// decides severity is in the payload: a `tool/result` is the same type
/// whether the tool worked or not. By the time an exporter sees a batch, the
/// chance to look has gone.
///
/// Input: the outcomes whose severity differs.
/// Expected: a failed tool result and a badly ended turn are `Error`; a
/// successful result, a natural end and an ordinary event are `Info`.
#[test]
fn severity_is_read_from_the_outcome() {
    assert_eq!(
        severity_of(&event("tool/result", json!({ "ok": false }))),
        Severity::Error
    );
    assert_eq!(
        severity_of(&event("tool/result", json!({ "ok": true }))),
        Severity::Info
    );
    for reason in ["failed", "timed-out", "repeated"] {
        assert_eq!(
            severity_of(&event("turn/end", json!({ "stop_reason": reason }))),
            Severity::Error,
            "{reason} is a turn that went wrong"
        );
    }
    for reason in ["natural", "interrupted", "max-tokens"] {
        assert_eq!(
            severity_of(&event("turn/end", json!({ "stop_reason": reason }))),
            Severity::Info,
            "{reason} is a turn that ended"
        );
    }
    assert_eq!(
        severity_of(&event("assistant/message", json!({ "content": "hello" }))),
        Severity::Info
    );
}

/// TC-PORT-TELEM-4: the seam ships no rules, and the rules a deployment mounts
/// stack.
///
/// The workspace cannot know which of a deployment's tool arguments are
/// secret, so it ships nothing and promises one place to say so. Stacking
/// matters because a general rule and a specific one are written by different
/// people at different times, and neither can be asked to know about the
/// other.
///
/// Input: no rules, then two that each rewrite part of a payload.
/// Expected: the record arrives untouched with no rules; both rewrites are
/// present with two, in registration order.
#[test]
fn the_redaction_seam_ships_nothing_and_stacks() {
    let sink = Arc::new(RecordingSink::new());
    let telemetry = Telemetry::new("s-1", Arc::clone(&sink) as Arc<_>);

    telemetry.capture(TelemetryRecord::ledger(
        "s-1",
        &event(
            "tool/call",
            json!({ "token": "sk-live", "path": "/home/me/x" }),
        ),
    ));
    assert_eq!(
        sink.records()[0].body["token"],
        "sk-live",
        "with no rules mounted, a record is exported as captured"
    );

    telemetry.redact(|mut record| {
        if record.body.get("token").is_some() {
            record.body["token"] = json!("<redacted>");
        }
        Some(record)
    });
    telemetry.redact(|mut record| {
        if let Some(path) = record.body.get("path").and_then(|v| v.as_str()) {
            record.body["path"] = json!(path.replace("/home/me", "~"));
        }
        Some(record)
    });

    telemetry.capture(TelemetryRecord::ledger(
        "s-1",
        &event(
            "tool/call",
            json!({ "token": "sk-live", "path": "/home/me/x" }),
        ),
    ));
    let cleaned = &sink.records()[1];
    assert_eq!(cleaned.body["token"], "<redacted>");
    assert_eq!(
        cleaned.body["path"], "~/x",
        "the second rule saw the first's work"
    );
}

/// TC-PORT-TELEM-5: a rule may withhold a record entirely.
///
/// Some records should not leave at all - a payload a deployment cannot clean
/// field by field is one. Withholding is `None` rather than an empty body,
/// because an exported record with its contents removed still says that this
/// session did that thing at that moment.
///
/// Input: a rule that drops one event type.
/// Expected: that type never reaches the sink, and the rest do.
#[test]
fn a_rule_may_withhold_a_record() {
    let sink = Arc::new(RecordingSink::new());
    let telemetry = Telemetry::new("s-1", Arc::clone(&sink) as Arc<_>);
    telemetry.redact(|record| (record.ty != "tool/call").then_some(record));

    telemetry.capture(TelemetryRecord::ledger(
        "s-1",
        &event("tool/call", json!({})),
    ));
    telemetry.capture(TelemetryRecord::ledger(
        "s-1",
        &event("turn/end", json!({})),
    ));

    assert_eq!(
        sink.records()
            .iter()
            .map(|r| r.ty.as_str())
            .collect::<Vec<_>>(),
        ["turn/end"]
    );
}

/// TC-PORT-TELEM-6: a rule that panics withholds its record and nothing else.
///
/// Fail-closed, and the reason is that the alternative is the one outcome
/// nobody would choose: exporting the record a rule was in the middle of
/// cleaning. The second half matters as much - this runs on the append path,
/// so a rule that indexed a payload that was shaped differently this time must
/// not fail the turn that was merely writing to its journal.
///
/// Input: a rule that panics on one event type, driven through a live journal.
/// Expected: the append succeeds, the offending record is withheld, and later
/// records are still captured.
#[test]
fn a_panicking_rule_withholds_its_record_and_nothing_else() {
    let (_home, bus, log) = journal("s-panic");
    let sink = Arc::new(RecordingSink::new());
    let telemetry = Telemetry::new("s-panic", Arc::clone(&sink) as Arc<_>);
    telemetry.redact(|record| {
        assert!(record.ty != "tool/call", "a rule with a bug");
        Some(record)
    });
    let _watch = telemetry.watch(&bus);

    let appended = log.append("tool/call", json!({ "name": "echo" }));
    assert!(
        appended.is_ok(),
        "a redaction rule's bug must not fail the append: {appended:?}"
    );
    log.append("turn/end", json!({ "stop_reason": "natural" }))
        .expect("appended");

    let types: Vec<String> = sink.records().into_iter().map(|r| r.ty).collect();
    assert!(
        !types.contains(&"tool/call".to_string()),
        "the record the rule panicked on was exported anyway: {types:?}"
    );
    assert!(
        types.contains(&"turn/end".to_string()),
        "and capture carried on afterwards: {types:?}"
    );
}

/// TC-PORT-TELEM-7: redaction touches the export and never the journal.
///
/// A log edited to satisfy an exporter is no longer a record of what happened,
/// and the harness would have destroyed the evidence it exists to keep. The
/// exported copy is the only thing a rule can change.
///
/// Input: a rule that rewrites a payload, over a real journal, read back off
/// the file.
/// Expected: the sink has the cleaned value and the file has the original.
#[test]
fn redaction_never_rewrites_the_journal() {
    let (home, bus, log) = journal("s-clean");
    let sink = Arc::new(RecordingSink::new());
    let telemetry = Telemetry::new("s-clean", Arc::clone(&sink) as Arc<_>);
    telemetry.redact(|mut record| {
        if record.body.get("token").is_some() {
            record.body["token"] = json!("<redacted>");
        }
        Some(record)
    });
    let _watch = telemetry.watch(&bus);

    log.append("tool/call", json!({ "token": "sk-live" }))
        .expect("appended");

    let exported = sink
        .records()
        .into_iter()
        .find(|r| r.ty == "tool/call")
        .expect("captured");
    assert_eq!(exported.body["token"], "<redacted>");

    let on_disk = tetanus_session::replay(home.path().join("t.jsonl")).expect("the journal");
    let stored = on_disk
        .iter()
        .find(|event| event.ty == "tool/call")
        .expect("the event");
    assert_eq!(
        stored.data["token"], "sk-live",
        "the journal was rewritten to suit an exporter"
    );
}

/// TC-PORT-TELEM-8: capture stops when the watch is dropped.
///
/// A registration is an effect here as everywhere else, and a telemetry
/// listener that outlived its owner would keep exporting a session nobody is
/// watching - to a sink whose collector may well be gone.
///
/// Input: appends before and after the handle is dropped.
/// Expected: only the first is captured.
#[test]
fn capture_stops_with_its_handle() {
    let (_home, bus, log) = journal("s-stop");
    let sink = Arc::new(RecordingSink::new());
    let telemetry = Telemetry::new("s-stop", Arc::clone(&sink) as Arc<_>);
    let watch = telemetry.watch(&bus);

    log.append("turn/start", json!({ "turn": 1 }))
        .expect("appended");
    let before = sink.records().len();
    drop(watch);
    log.append("turn/end", json!({ "stop_reason": "natural" }))
        .expect("appended");

    assert_eq!(
        sink.records().len(),
        before,
        "capture continued after its handle was dropped"
    );
}
