//! The capture side of session telemetry: which records exist, what they
//! carry, when they are taken, and the one place a deployment gets to redact
//! them before they leave.
//!
//! **Capture only, deliberately.** Everything downstream of
//! [`TelemetrySink::emit`] - batching, retry, queueing, what to do when a
//! collector is down - belongs to whatever reporting SDK a deployment
//! chooses, and is not modelled here. That is upstream's split too
//! (`packages/session/session-telemetry` against
//! `session-telemetry-otel`), and it is what lets this land with no
//! dependency at all: taking OpenTelemetry into the workspace is a decision
//! about a transitive tree, and it is not the same decision as deciding what a
//! record is.
//!
//! **Two channels that cannot be mistaken for each other.** A `ledger` record
//! mirrors one session-log event, one for one, and carries that event's
//! identity. An `ops` record carries a signal with no home on the log - a
//! failure the harness itself hit, a shutdown - and carries no `seq`, so a
//! receiver counting ledger rows against the journal never counts one of
//! these among them.
//!
//! **Severity is mapped at capture.** A receiver should be able to alert with
//! no configuration, and the facts that decide severity - a tool result that
//! failed, a turn that ended badly - are on the record at capture and gone by
//! the time an exporter sees a batch.
//!
//! **Redaction is a seam that ships no rules.** The workspace cannot know
//! which of a deployment's tool arguments are secret. What it can promise is
//! one place to say so, applied to every record on the way out, and that the
//! export is the only copy affected: the canonical journal is never rewritten,
//! because a log that was edited to satisfy an exporter is no longer a record
//! of what happened.
//!
//! **Fail-closed.** A redaction rule that panics withholds that record. The
//! alternative - exporting the record the rule was in the middle of cleaning -
//! is the one outcome nobody would choose, and a panicking rule must never
//! reach the turn that was merely writing to its journal.
//!
//! Parity: upstream `packages/session/session-telemetry`, its record shape,
//! channels, severity mapping and `session-telemetry/record` waterfall.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::{SessionEvent, SessionEventDispatch};
use tetanus_core::{EffectHandle, EventBus};

/// How urgent a record is, decided where the facts are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// Which of the two vocabularies a record belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// A mirror of one session-log event.
    Ledger,
    /// A signal with no home on the log.
    Ops,
}

/// One record on its way out of the process.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TelemetryRecord {
    pub channel: Channel,
    /// The source event's append time for a ledger record, the emission time
    /// for an ops one.
    pub time: u64,
    pub severity: Severity,
    /// The session this belongs to. Present on both channels: an operational
    /// failure nobody can attribute to a session is a failure nobody can act
    /// on.
    pub session_id: String,
    /// The event type for a ledger record, the signal's name for an ops one.
    pub ty: String,
    /// The log position, on ledger records only. `None` is what makes an ops
    /// record impossible to mistake for a row of the journal.
    pub seq: Option<u64>,
    pub body: Value,
}

impl TelemetryRecord {
    /// The record one journal event becomes.
    pub fn ledger(session_id: &str, event: &SessionEvent) -> Self {
        Self {
            channel: Channel::Ledger,
            time: event.time,
            severity: severity_of(event),
            session_id: session_id.to_string(),
            ty: event.ty.clone(),
            seq: Some(event.seq),
            body: event.data.clone(),
        }
    }

    /// A signal with no home on the log.
    pub fn ops(session_id: &str, ty: &str, severity: Severity, time: u64, body: Value) -> Self {
        Self {
            channel: Channel::Ops,
            time,
            severity,
            session_id: session_id.to_string(),
            ty: ty.to_string(),
            seq: None,
            body,
        }
    }
}

/// The severity a journal event carries out of the process.
///
/// Read from the event's own outcome rather than from its type, because the
/// type says what happened and only the payload says whether it went well: a
/// `tool/result` is the same type whether the tool worked or not.
pub fn severity_of(event: &SessionEvent) -> Severity {
    let failed_tool =
        event.ty == "tool/result" && event.data.get("ok") == Some(&Value::Bool(false));
    let failed_turn = event.ty == "turn/end"
        && matches!(
            event.data.get("stop_reason").and_then(Value::as_str),
            Some("failed" | "timed-out" | "repeated")
        );
    match failed_tool || failed_turn {
        true => Severity::Error,
        false => Severity::Info,
    }
}

/// Where records go once they are captured and cleaned.
///
/// One method, because everything that makes a good exporter - batching,
/// backpressure, retry - is the exporter's and modelling it here would be
/// modelling somebody else's SDK badly.
pub trait TelemetrySink: Send + Sync {
    fn emit(&self, record: TelemetryRecord);
}

/// A sink that keeps what it was given, for a deployment with no collector and
/// for the cases in this crate.
#[derive(Default)]
pub struct RecordingSink {
    records: Mutex<Vec<TelemetryRecord>>,
}

impl RecordingSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn records(&self) -> Vec<TelemetryRecord> {
        self.records.lock().expect("recorded telemetry").clone()
    }
}

impl TelemetrySink for RecordingSink {
    fn emit(&self, record: TelemetryRecord) {
        self.records
            .lock()
            .expect("recorded telemetry")
            .push(record);
    }
}

/// One deployment's rule for what may leave. Answers the record to export, or
/// `None` to withhold it entirely.
pub type Redaction = Arc<dyn Fn(TelemetryRecord) -> Option<TelemetryRecord> + Send + Sync>;

/// Capture: journal appends in, records out, with the deployment's rules
/// applied on the way.
pub struct Telemetry {
    session_id: String,
    sink: Arc<dyn TelemetrySink>,
    rules: Mutex<Vec<Redaction>>,
}

impl Telemetry {
    pub fn new(session_id: impl Into<String>, sink: Arc<dyn TelemetrySink>) -> Arc<Self> {
        Arc::new(Self {
            session_id: session_id.into(),
            sink,
            rules: Mutex::new(Vec::new()),
        })
    }

    /// Add a rule. Rules run in registration order, each seeing what the last
    /// one returned, so a deployment can stack a general rule and a specific
    /// one without either knowing about the other.
    pub fn redact<F>(&self, rule: F)
    where
        F: Fn(TelemetryRecord) -> Option<TelemetryRecord> + Send + Sync + 'static,
    {
        self.rules
            .lock()
            .expect("redaction rules")
            .push(Arc::new(rule));
    }

    /// Clean and emit one record, or withhold it.
    ///
    /// A rule that panics withholds the record and does not unwind into the
    /// caller: this runs on the append path, and a turn must not fail because
    /// somebody's redaction rule indexed a payload that was shaped differently
    /// this time.
    pub fn capture(&self, record: TelemetryRecord) {
        let rules = self.rules.lock().expect("redaction rules").clone();
        let mut carried = Some(record);
        for rule in rules {
            let Some(current) = carried.take() else {
                return;
            };
            match catch_unwind(AssertUnwindSafe(|| rule(current))) {
                Ok(answer) => carried = answer,
                Err(_) => {
                    tracing::warn!("a telemetry redaction rule panicked; the record is withheld");
                    return;
                }
            }
        }
        if let Some(record) = carried {
            self.sink.emit(record);
        }
    }

    /// Capture every append on `bus` for the lifetime of the returned handle.
    ///
    /// The per-append firehose: telemetry is a *reader* of the journal rather
    /// than a second writer, so a record exists exactly when an event does and
    /// nothing has to remember to report.
    pub fn watch(self: &Arc<Self>, bus: &EventBus) -> EffectHandle {
        let held = Arc::clone(self);
        bus.on_emit::<SessionEventDispatch>(move |dispatch| {
            held.capture(TelemetryRecord::ledger(&held.session_id, &dispatch.event));
        })
    }

    /// Report something that has no home on the journal.
    pub fn report(&self, ty: &str, severity: Severity, body: Value) {
        self.capture(TelemetryRecord::ops(
            &self.session_id,
            ty,
            severity,
            crate::now_ms(),
            body,
        ));
    }
}
