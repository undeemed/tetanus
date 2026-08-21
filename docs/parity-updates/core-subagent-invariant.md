# Parity update — delegation lifecycle invariants

Slice: `core-subagent-invariant`. Lane: engine/core (phase ②, subagent block).

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/subagent/subagent/src/invariant.ts`, `tests/invariant.spec.ts` | `crates/subagent/src/invariant.rs`, `crates/subagent/tests/invariant.rs` | TC-SUB-INV-1..11 |

## Structural difference — a fold, as in the two places before it

Upstream registers a cordis companion that refuses a bad dispatch. This
workspace has no invariant registry; `upstream_session_invariants.rs` records
that choice for the session store and `crates/hooks/src/invariant.rs` follows
it, so this does too. Each of upstream's throws becomes a reported fault.

The fold makes one difference visible that upstream's code hides:
`validateRunEnd` is called even when no matching start was found. In upstream
that is unreachable, because the missing-start failure throws first. A fold
that reports everything would otherwise attach a second, meaningless
"identity diverges" fault to every orphaned end, so the identity check is
skipped when there is no beginning to diverge from. TC-SUB-INV-11 pins it.

## A deliberate absence, now pinned

Provider availability is checked when a run is **admitted**, never for the
run's whole life. A one-shot run may outlive the removal of the provider that
started it, and a resumed run records the provider it began under without
dispatching through it again. Upstream says so in a comment; TC-SUB-INV-10
turns that comment into a case, because a stricter check here would report
faults on both of those correct behaviours.

## Cases beyond the upstream suite

- **TC-SUB-INV-9** — a run id is reusable once its run has closed. The registry
  tracks *open* runs, and a backend numbering runs per child would legitimately
  reuse an id.
- **TC-SUB-INV-10** — the deliberate absence above.
- **TC-SUB-INV-11** — an orphaned ending reports one fault, not two.

## Shape

The fold is a four-arm dispatch over one helper per event, rather than one
function holding every rule. It was written the other way first and the
structural gate caught it: 21 branches in one function took the workspace's
complex-function count from 11 to 12. Splitting it restored the count and the
rules read as a list of what can happen.

## Changelog row

| 2026-08-21 | Delegation lifecycle invariants ported (`crates/subagent/src/invariant.rs`, TC-SUB-INV-1..11): the provider-registry and run-pairing rules the parent's bookkeeping rests on, since a run that starts twice or ends without starting corrupts it silently rather than loudly. The subtle rule is the last: `subagent/end` repeats a run's whole identity rather than only its id, and if the repeat disagrees then one of the two events is about a different run, which means a parent may be crediting an answer to the wrong child. As with the hooks block and the session store before it, the rules fold over a recorded stream instead of refusing a dispatch, because this workspace has no invariant registry. That fold exposes something upstream's code hides: it validates the end identity even when no start was found, which is unreachable there because the missing-start failure throws first, so the identity check is skipped here to keep every orphaned end from carrying a second meaningless fault. Three cases go beyond upstream: a run id is reusable once its run closed, since the registry tracks open runs; a run may outlive the removal of the provider that started it, which upstream states in a comment and which a stricter check would wrongly report; and an orphaned ending reports one fault rather than two. |
