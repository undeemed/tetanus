# Parity note: the bounds a deployment sets on a turn

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.

Branch: `fm/tetanus-p8-ctx`.
Scope: the `context/*`, `guard/*` row's "timeout and repeat guards".

---

## 1. What was built

`crates/turn/src/guard.rs`, TC-GUARD-1, -5..8 and TC-PORT-GUARD-2..4: a
whole-turn time budget and a repeat detector, read at the step boundary, ending
the turn with `"timed-out"` or `"repeated"`.

This is another published-with-no-producer closure, the fifth in these areas.
Contract section 4.4.2 has specified both reasons since the guards clause
landed - what the journal carries, that the prompt still answers a summary,
that a guard stops at a step boundary - and `tetanus_turn::StopReason` had
neither variant, so no turn could ever end that way.

## 2. Where this departs from upstream, deliberately

Upstream's `guard/` packages are the nearest relatives and are weaker on
purpose. `timeout-policy` bounds one **tool call** and maps its expiry to a
`TOOL_TIMEOUT` result; `repeat-tool-reminder` counts consecutive identical
calls and **adds a reminder message**, vetoing nothing.

tetanus takes upstream's detection rule - consecutive calls of the same tool
with canonically identical arguments - and pairs it with the action its own
contract already published: end the turn and say which guard did it. The
contract's argument is that the two reasons need opposite answers, which only
works if the turn actually stops; a reminder leaves the loop running.

Three rules the implementation settles, each with a case:

- **The unit of repetition is the batch a step asked for, not one call.** A
  model alternating two tools is looping, and a per-call counter resets on
  every alternation and never fires.
- **The call id is not part of the comparison.** A provider mints a fresh id
  per call, so a detector that compared ids would find every call unique - the
  guard would be dead code that looked alive.
- **A limit below two is read as no limit.** A limit of one stops a turn on its
  first tool call; that is a configuration mistake, and answering it with a
  turn that can never use a tool is worse than ignoring it.

The clock is monotonic (`Instant`), not a difference of journal stamps: the
contract says in as many words that subtracting two `time` values is an
estimate that is occasionally negative, and a bound NTP can defeat is not a
bound.

## 3. Row edit, section 3

**`context/*`, `guard/*`.** Gap: remove `and timeout and repeat guards`,
leaving `A tmux provider`. Today: add `the two bounds a deployment sets on a
turn rather than on a request - a whole-turn budget and a repeat detector, both
read at the step boundary so a guarded turn keeps a balanced journal`.

## 4. Row edit, section 4

| Row | Edit |
| --- | --- |
| `guard/timeout-policy`, `guard/repeat-tool-reminder` | New row -> `crates/turn/tests/upstream_guards.rs`. Part ported: TC-PORT-GUARD-2..4 restate the detection rule (consecutive identical calls, compared by tool and canonical arguments rather than by call id). The *actions* deliberately differ and the row should say so: upstream bounds one tool call and reminds a looping model, where contract section 4.4.2 already published a turn-level stop, so tetanus ends the turn with `"timed-out"` or `"repeated"`. Upstream's per-call timeout has no counterpart here and needs one: a tool that runs forever is still only bounded by the turn. |

## 5. What is left

- **A per-tool-call timeout.** Upstream's `timeout-policy` proper. The turn
  budget bounds a runaway tool only by ending the whole turn, which is a
  blunter answer than a `TOOL_TIMEOUT` result the model can read and work
  around. It belongs with whoever owns the tool pipeline.
- **A tmux provider** (`context/*`), unchanged: still a decision about shelling
  out to a program that may not be installed, not code.

## 6. Changelog row

| 2026-08-25 | The two bounds a deployment sets on a turn rather than on a request (`crates/turn/src/guard.rs`, TC-GUARD-1, -5..8 and TC-PORT-GUARD-2..4). Contract section 4.4.2 has published `"timed-out"` and `"repeated"` since the guards clause landed - what the journal carries, that the prompt still answers a summary rather than an error, that a guard stops at a step boundary - and `tetanus_turn::StopReason` had neither variant, so no turn could end that way: the fifth published-with-no-producer gap found in these areas. The provider seam already bounds one request with an idle window and a deadline, and neither bounds a turn: a model that answers promptly and calls one more tool every time is inside every per-request bound there is while getting nowhere. Guards are read at the step boundary, where an interrupt lands and for the same reason - a dispatched step has already had its effect - so a guarded turn is a whole turn with a balanced journal and section 4.6's state machine holds unchanged. The two reasons stay separate because they need opposite answers: `"timed-out"` asks for a bigger budget or a smaller task, and a bigger budget makes `"repeated"` strictly worse. Upstream is the nearest relative and is weaker on purpose - its `timeout-policy` bounds one tool call and its `repeat-tool-reminder` only adds a reminder - so what ports is the detection rule and the action is the one this contract already chose. Three rules carry their own cases: the unit of repetition is the batch a step asked for rather than one call, since a model alternating two tools is looping and a per-call counter would reset on every alternation; the call id is not part of the comparison, because a provider mints a fresh one per call and a detector comparing ids would be dead code that looked alive; and a limit below two is read as no limit, because answering a configuration mistake with a turn that can never use a tool is worse than ignoring it. The clock is monotonic, not a difference of journal stamps, which the contract itself calls an estimate that is occasionally negative. What is left is upstream's per-call timeout, which belongs with the tool pipeline: a runaway tool is still bounded only by ending the whole turn. |
