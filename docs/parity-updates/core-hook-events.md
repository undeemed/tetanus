# Parity update — the `hook/*` journal events

Slice: `core-hook-events`. Lane: engine/core (phase ②, hooks block).

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/hooks/hook-protocol/src/events.ts`, `tests/events.spec.ts` | `crates/hooks/src/events.rs`, `crates/hooks/tests/events.rs` | TC-HOOK-EVENT-1..12 |

Contract side in `docs/contract-updates/core-hook-events.md`: two additive
log-only journal event types, no `crates/protocol` change.

## Section 5 — deliberate difference

**The stderr cap counts characters, not UTF-16 code units.** Upstream caps with
`String.prototype.slice`, which counts UTF-16 units, so a summary of
astral-plane text is cut at a different point there. Counting *bytes* would be
worse than either: it panics mid-character, which is the defect the tool-result
pruner's port already had to fix. This counts what a reader would count.

TC-HOOK-EVENT-11 pins it over two-byte, three-byte and surrogate-pair text, and
the mutation check confirms it bites — swapping to byte slicing fails it.

## Cases beyond the upstream suite

- **TC-HOOK-EVENT-11** — the cap counts characters and a cut never lands inside
  one. Upstream cannot ask this; stderr is the likeliest place for a hook to
  print something outside ASCII.
- **TC-HOOK-EVENT-12** — a zero cap yields `…`, not `None`. The cap comes from
  configuration so zero is reachable, and the failure to avoid is silently
  reporting no stderr at all, which reads as a hook that printed nothing.

## Changelog row

| 2026-08-21 | The `hook/*` journal events ported (`crates/hooks/src/events.rs`, TC-HOOK-EVENT-1..12): the `hook/invoked`/`hook/result` pair that makes what a deployment's hooks did auditable after the fact. They are log-only and turn-enclosed, correlated by `handlerId`, and written by the protocol rather than by each adapter so the two dialects cannot drift on what a result means. Three payload rules each answer a question a reader would otherwise have to guess: `matcher` is omitted rather than null for a match-all hook, because "matched everything" and "matched this pattern" are different facts; `exitCode` is omitted when the hook could not be run, so a failed spawn cannot read as a clean exit; and `decision` is always present, falling back to `stop` for a halt and `pass` for silence, so nobody has to infer "nothing happened" from an absent field. The stderr summary is trimmed, capped and marked when cut, and the cap counts characters - upstream counts UTF-16 units and counting bytes would panic mid-character, which is the defect the tool-result pruner's port already fixed. Two cases go beyond upstream: the cap is exercised over two-byte, three-byte and surrogate-pair text, and a zero cap is asserted to still report that something was printed. A mutation check confirms the suite bites: byte slicing, dropping the `stop` fallback, always writing the matcher key, and making the cap inclusive each fail one or two cases. |
