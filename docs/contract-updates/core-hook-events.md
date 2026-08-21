# Contract update — the `hook/*` journal events

Slice: `core-hook-events`. Lane: engine/core (phase ②, hooks block).

Proposed additions to `docs/interface-contract.md`, held here rather than
edited into the shared doc while four lanes land in parallel. **Nothing in
`crates/protocol` changes**: these are journal event types, which the contract
describes by name and payload, not Rust types a client matches on.

## §4.3.1 — two new event types

Both are **log-only**. No client rendering depends on them, nothing in a turn
reads them back, and neither carries `sourceEventSeqs`. They exist so that "why
was my tool call denied" has an answer on the journal.

Both are **turn-enclosed** and appear as an **invoked/result pair** correlated
by `handlerId`.

| Event | Payload |
| --- | --- |
| `hook/invoked` | `turn`, `point`, `dialect`, `handlerId`, plus `matcher` when the hook was selected by a pattern |
| `hook/result` | `turn`, `point`, `handlerId`, `decision`, `durationMs`, plus `exitCode` when the process ran, plus `stderrSummary` when it printed anything |

### Field notes

- `dialect` is `claude-code` or `codex` — which bridge ran the hook. A native
  plugin at the same interception point is not a bridge and writes no `hook/*`
  events at all.
- `matcher` is **omitted, never null**, for a match-all hook. "Matched
  everything" and "matched this pattern" are different facts and a null would
  blur them.
- `decision` is always present: the hook's permission answer if it gave one,
  otherwise `stop` when it asked to halt, otherwise `pass`. A reader never has
  to infer "nothing happened" from an absent field.
- `exitCode` is **omitted** when the hook could not be run. Absent and `0` mean
  different things, and a failed spawn must not read as a clean exit.
- `stderrSummary` is trimmed, capped, and marked with `…` when cut. Omitted
  when the hook printed nothing.

## Compatibility

Additive, and safe under all four §5 rules:

1. No existing event type or payload changes.
2. No `ErrorCode` or protocol enum gains a variant, so no exhaustive match
   anywhere breaks.
3. A client that does not know these types already ignores unknown event types
   — that is the existing rule for the journal vocabulary.
4. No consumer destructures them, because nothing produced them until now.

No changelog row is proposed for the contract doc until this lands, to avoid
four lanes appending to the same table.
