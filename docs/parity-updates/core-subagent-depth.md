# Parity update — delegation depth

Slice: `core-subagent-depth`. Lane: engine/core (phase ②, subagent block).

Opens `crates/subagent` and the `subagent/*` rows.

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/subagent/subagent/src/depth.ts`, plus the depth rules of `service.spec.ts` and `continuation.spec.ts` | `crates/subagent/src/depth.rs`, `crates/subagent/tests/depth.rs` | TC-SUB-DEPTH-1..11 |

## Structural difference — validation moved to the boundary

Upstream validates a depth at every use, because a JavaScript number can be
`-0`, `NaN` or `1.5`. Here the in-memory type is `u64`, so none of those are
representable and the equivalent checks would be unreachable code.

The validation that remains sits where those values can still arrive: reading a
depth out of a JSON configuration document, in `depth_from_json`. That is the
same rule enforced once at the edge instead of repeatedly inside, and the error
message is upstream's.

## Cases beyond the upstream suite

- **TC-SUB-DEPTH-9** — an unset cap is not a cap of zero. This absent-field
  distinction runs the opposite way from most: zero *forbids* delegation, so
  reading "unset" as zero would silently disable every subagent, while unset
  means unlimited.
- **TC-SUB-DEPTH-10** — a whole number written as a float is accepted, because
  JSON does not distinguish `2` from `2.0` and a tool-written settings file
  would otherwise have every depth refused. `2.5` is still refused: that is a
  mistake, not a spelling.
- **TC-SUB-DEPTH-11** — the count saturates rather than wrapping. A wrap would
  turn the deepest possible child into a top-level agent with a full budget,
  which is exactly the failure the monotone rule prevents, reached by
  arithmetic instead.

## Changelog row

| 2026-08-21 | Delegation depth ported (`crates/subagent/src/depth.rs`, TC-SUB-DEPTH-1..11), opening `crates/subagent` and the `subagent/*` rows. An agent can start a child that can start its own, and without a budget that recursion has no floor - a persona that always delegates would spawn agents until the machine stopped, each spending real money on real model calls. The rule carrying the safety is that depth is monotone: it is the larger of the persisted header value and the runtime one, so runtime may deepen the count and can never shorten it. That is not a tie-break. A resumed child is constructed with fresh options, and believing the runtime value outright would let a child resumed with nothing set count itself as top-level and delegate on a full budget; the mutation check confirms it, since preferring the runtime value fails that case. Upstream validates at every use because a JS number can be `-0`, `NaN` or `1.5`; here the type is `u64` and those are unrepresentable, so the same rule is enforced once at the JSON boundary where such values can still arrive. Three cases go beyond upstream: an unset cap is unlimited rather than zero, since zero forbids delegation and reading unset as zero would silently disable every subagent; a whole number written as a float is accepted because JSON does not distinguish `2` from `2.0`; and the count saturates rather than wrapping, a wrap being the same top-level-budget failure reached by arithmetic. |
