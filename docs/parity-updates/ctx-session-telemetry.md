# Parity note: session telemetry, the half that needs no dependency

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.

Branch: `fm/tetanus-p6-ctx-rows`.
Scope: the `session/*` row's "telemetry", which the sweep earlier in this
branch called a dependency decision. Half of it was; this is the half that was
not.

---

## 1. What was built

`crates/session/src/telemetry.rs`, TC-PORT-TELEM-1..8: records, channels,
severity, the redaction seam, and capture off the append firehose.

Upstream splits telemetry into two packages and says exactly where the line
is: `session-telemetry` owns "which records exist, what they carry, when they
are captured", and everything downstream of `emit` - batching, retry, queueing,
loss policy - "is the reporting SDK's territory and is deliberately not
modelled here". Only the second half needs OpenTelemetry. Reading the sweep's
note again against that split showed the first half is a port with no
dependency in it at all, so it is done and the row's remaining clause is now a
named exporter rather than "telemetry".

## 2. The decisions worth recording

- **Two channels, one of which carries no `seq`.** A receiver counting
  exported rows against a session's journal must get the same number, so an
  operational signal - a harness failure, a shutdown - carries no log position
  rather than a plausible one.
- **Severity at capture.** The fact that decides it is in the payload (`ok:
  false` on a tool result, a `stop_reason` of `failed`, `timed-out` or
  `repeated`), and by the time an exporter sees a batch the chance to look has
  gone. A receiver can then alert with no configuration.
- **A seam that ships no rules.** The workspace cannot know which of a
  deployment's tool arguments are secret. What it can promise is one place to
  say so and that rules stack in registration order, since a general rule and a
  specific one are written by different people who cannot be asked to know
  about each other.
- **Fail-closed, and containment.** A rule that panics withholds its record -
  exporting the record a rule was in the middle of cleaning is the one outcome
  nobody would choose - and does not unwind into the caller, because this runs
  on the append path and a turn must not fail over somebody's redaction rule.
- **Redaction touches the export only.** A journal rewritten to satisfy an
  exporter is no longer a record of what happened. TC-PORT-TELEM-7 reads the
  file back to prove it.
- **Capture is not retroactive.** It begins where the watch is attached, so a
  `session/start` written by `create` is not among the records. Inventing one
  would put a time on the wire that no listener ever saw.

## 3. Row edits

**Section 3, `session/*`.** Gap: replace `telemetry` with `an exporter for the
telemetry capture side, which is an OpenTelemetry dependency decision`. Today:
add `telemetry capture - ledger and ops records, severity mapped at capture,
and a redaction seam that ships no rules and cannot fail a turn`.

**Section 4.** New row: `session/session-telemetry/tests/telemetry.spec.ts`,
`redact.spec.ts` -> `crates/session/tests/upstream_telemetry.rs`, ported as
TC-PORT-TELEM-1..8. Upstream's HMR cursor and its live-versus-on-demand
canonical-log capture have nothing to restate: tetanus has no module reload,
and its journal is readable at any time by `replay`, so "capture on demand" is
reading the file.

## 4. Changelog row

| 2026-08-22 | Session telemetry capture (`crates/session/src/telemetry.rs`, TC-PORT-TELEM-1..8). The sweep earlier in this branch called telemetry a dependency decision; half of it was. Upstream splits it into two packages and states the line - one owns which records exist, what they carry and when they are taken, and everything downstream of `emit` is the reporting SDK's - and only that second half needs OpenTelemetry. A ledger record mirrors one journal event and carries its position; an ops record carries a signal with no home on the log and carries no position at all, so a receiver counting exported rows against a journal gets the same number. Severity is mapped at capture because the fact that decides it is in the payload - a tool result that failed, a turn that ended `failed`, `timed-out` or `repeated` - and is gone by the time an exporter sees a batch. The redaction seam ships no rules, because the workspace cannot know which of a deployment's tool arguments are secret; what it promises is one place to say so, rules that stack in registration order, and two safety properties that are the whole reason the seam is here rather than in an exporter: a rule that panics withholds its own record and nothing else, since exporting the record a rule was in the middle of cleaning is the one outcome nobody would choose, and it does not unwind into the caller, since this runs on the append path and a turn must not fail over somebody's redaction bug. Redaction touches the exported copy only - a journal rewritten to satisfy an exporter is no longer a record of what happened - and TC-PORT-TELEM-7 reads the file back to prove it. Capture is not retroactive: it begins where the watch is attached, so the `session/start` line `create` wrote is not among the records, because inventing one would put a time on the wire that no listener ever saw. |
