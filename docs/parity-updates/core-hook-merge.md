# Parity update — hook decision merge

Slice: `core-hook-merge`. Lane: engine/core (phase ②, hooks block).

Rows for `docs/parity.md`, held here so four lanes can land in parallel without
colliding on that file.

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/hooks/hook-protocol/src/merge.ts`, `tests/merge.spec.ts` | `crates/hooks/src/merge.rs`, `crates/hooks/tests/merge.rs` | TC-HOOK-MERGE-1..14 |
| `packages/hooks/hook-protocol/src/types.ts` (decision vocabulary) | `crates/hooks/src/types.rs` | covered by the above |

`types.ts` is ported only as far as the merge needs it. The wire decoding of
these types is the codec slice, and the event vocabulary is its own.

## Deviation worth recording — not a behaviour difference

Upstream ranks decisions with a `number` and maps back with a `switch`.
`MergedDecision` here is an ordered enum declared least-restrictive first, so
the fold is `max` and "most restrictive wins" is a property of the type rather
than two switches that have to agree. The mutation check exercises this
directly: swapping `Ask` and `Deny` in the declaration fails two cases, so the
ordering is pinned by the suite and not merely by the declaration's comment.

## Cases beyond the upstream suite

- **TC-HOOK-MERGE-13** — the decision does not depend on the order answers
  arrive in, across every pair of the five spellings. Hooks run concurrently in
  a later slice, so arrival order stops matching configuration order; the
  decision must be a function of the set. Upstream's suite pins three specific
  orderings, which does not say this.
- **TC-HOOK-MERGE-14** — inserting a silent, observe-only hook at any position
  leaves the whole outcome identical. This is what a deployment relies on when
  it adds a logging hook to a point that already has a gate.

## Changelog row

| 2026-08-21 | The hook decision merge ported (`crates/hooks/src/merge.rs`, `src/types.rs`, TC-HOOK-MERGE-1..14). Several hooks can match one point and disagree, and the fold is deliberately not a vote: the most restrictive answer wins, because a hook exists to stop something and one that is outvoted has not been heard. Four rules, each with a separate reason and each defended by the mutation check: permission is `deny > ask > allow`, expressed as an ordering on the enum so the fold is `max` rather than two switches that must agree - swapping two variants in the declaration fails two cases; only the winning answer's reasons surface, because explaining a refusal with the text of an unrelated `allow` would misdescribe it; a halt is sticky and keeps the *first* halting hook's reason, later halts being implied by it; and context and warnings accumulate in hook order with empties skipped, unjoined, because what separates them belongs to whoever renders them. Two cases go beyond upstream's suite: the decision is order-independent across every pair of the five decision spellings, which matters because hooks run concurrently in a later slice and arrival order stops matching configuration order, and inserting a silent observe-only hook anywhere leaves the outcome identical, which is what a deployment relies on when it adds logging to a point that already has a gate. |
