# Parity update — settling a finished child run

Slice: `core-subagent-settlement`. Lane: engine/core (phase ②, subagent block).

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/subagent/subagent/src/run-settlement.ts`, `tests/run-settlement.spec.ts` | `crates/subagent/src/settlement.rs`, `crates/subagent/tests/settlement.rs` | TC-SUB-SETTLE-1..9 |

## Structural difference — settlement is a fold, not an await

Upstream's `settleRun` awaits the run's promise and calls its `dispose`. Here
`settle` takes the two results that awaiting produced. The ordering rule —
dispose before reporting — is then the caller's to keep, and it is a property
of the signature rather than of a comment: there is no outcome to report until
both results are in hand.

This keeps the module free of a runtime and testable without one, which is
what lets the whole matrix of result/disposal combinations be a table.

The `Killed` outcome carries **no output field at all**, so an aborted run
cannot report a partial draft as its answer even by mistake. Upstream relies on
the mapping never populating it; here it is unrepresentable.

## Cases beyond the upstream suite

- **TC-SUB-SETTLE-6** and **-7** — an aborted or failed run reports its reason,
  never the text the child had written. Upstream's fixtures happen to carry
  text through these paths without asserting it is dropped, and presenting a
  partial draft as a completed answer is the failure that matters.
- **TC-SUB-SETTLE-8** — a completed run with no output still completes. Empty
  output and failure are different facts; collapsing them turns a child that
  legitimately had nothing to say into a run the parent reports as broken.
- **TC-SUB-SETTLE-9** — a stop reason survives a round trip through its written
  form. It crosses a process boundary as text from an out-of-process backend,
  and a reason that did not round-trip would come back unknown, turning a clean
  completion into a failure named `completed`.

## Changelog row

| 2026-08-21 | Settling a finished child run ported (`crates/subagent/src/settlement.rs`, TC-SUB-SETTLE-1..9). A one-shot delegated run is a background task from the parent's side, and something has to say what happened and free what the child held. Two rules earn the module: disposal always happens, because a settlement that returned early on a bad result would leak a child process for exactly the population most likely to be numerous; and a failure to release does not hide the failure that caused it, so when both fail both survive with the cause first - reporting only the disposal error would replace the diagnosis with an after-effect. An unknown stop reason is a failure named by itself rather than a refusal to classify, because a run that stopped for an unrecognised reason has still stopped and leaving it unclassified would leave the parent waiting. The `Killed` outcome carries no output field at all, so an aborted run cannot report a partial draft as its answer; upstream relies on the mapping never populating it, here it is unrepresentable. Four cases go beyond upstream: an aborted and a failed run each report their reason and not the child's text, a completed run with no output still completes, and a stop reason round-trips through the text form it crosses a process boundary in. |
