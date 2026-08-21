# Parity update — hook output codec

Slice: `core-hook-codec`. Lane: engine/core (phase ②, hooks block).

Rows for `docs/parity.md`, held here so four lanes can land in parallel.

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/hooks/hook-protocol/src/codec.ts`, `tests/codec.spec.ts` | `crates/hooks/src/codec.rs`, `crates/hooks/tests/codec.rs` | TC-HOOK-CODEC-1..24 |

`types.ts` gains `hookEventName` and `updatedInput`, and `exitCode` becomes
`Option<i32>` so "could not be run" stays distinct from "ran and said nothing".

## Cases beyond the upstream suite

- **TC-HOOK-CODEC-23** — decoding is total over hostile output: twelve
  malformed payloads across all four exit states, asserting no panic and that a
  wrong-typed field reads as absent rather than being coerced. Upstream has
  `str`/`bool`/`obj` helpers that do this and no case that asks whether they do.
- **TC-HOOK-CODEC-24** — the event guard cannot be bypassed through the legacy
  channel. This is why TC-HOOK-CODEC-8 matters: the guard protects
  `permissionDecision`, so if the top-level `decision` also accepted `deny`, a
  hook could deny an event it was never fired for by writing the word one line
  higher. Upstream pins the two halves separately and never states the
  consequence.

## Named test gap — an equivalent mutant, not an untested rule

Upstream only attempts a JSON parse when trimmed stdout starts with `{`. That
guard is **not observable in this port** and no case pins it: after trimming, a
JSON object always starts with `{`, and `serde_json` rejects everything else
anyway, so removing the check changes no output. It is kept because it states
the intent — plain text is plain text, not a malformed answer — and because a
future non-`serde_json` reader could make it load-bearing.

The mutation check confirms this directly: removing the prefix check fails
nothing. Recording it is the point; a case asserting it would be asserting the
behaviour of `serde_json`, not of this module.

A second mutation was discarded as equivalent for the same kind of reason:
deleting the early `return` after an exit-2 block appears to let structured
stdout through, but the parse is also gated on `exit_code == Some(0)`, so
nothing changes. The real mutation — admitting exit 2 to the structured parse —
is caught by TC-HOOK-CODEC-22.

## Changelog row

| 2026-08-21 | The hook output codec ported (`crates/hooks/src/codec.rs`, TC-HOOK-CODEC-1..24). A hook answers in two channels at once, its exit status and whatever it printed, and decoding is deliberately total and lenient: a hook is someone else's program and a turn must not die because one of them printed something unexpected. The exit status frames everything - 0 may carry a structured answer, 2 is a block with stderr as the reason, anything else is an error that does not block, and none at all means it could not be run, which is kept distinct from running and saying nothing so a failed spawn cannot read as an approval. Exit 2 is authoritative and structured stdout is not even read on it, because a hook that both blocked and printed an approval has contradicted itself and the blocking channel is the one that fails closed. The event guard is the security-relevant half: a `hookSpecificOutput` block that names a different event, or names none, has its event-scoped fields discarded, so a stray `PreToolUse` denial cannot deny a `Stop` - while the claimed name is still recorded, because a diagnostic saying a block was discarded is only useful if it can say what the block claimed. Two cases go beyond upstream: decoding is total over twelve hostile payloads across all four exit states, and the event guard cannot be bypassed by writing `deny` at the top level, which is the consequence upstream pins in two halves and never states. One named non-observable: the `starts_with('{')` fast path cannot be mutation-detected here, because a trimmed JSON object always starts with `{` and `serde_json` rejects the rest regardless. |
