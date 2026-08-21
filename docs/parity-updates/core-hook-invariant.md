# Parity update — hook stream invariants

Slice: `core-hook-invariant`. Lane: engine/core (phase ②, hooks block).

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/hooks/hook-protocol/src/invariant.ts`, `tests/invariant.spec.ts` | `crates/hooks/src/invariant.rs`, `crates/hooks/tests/invariant.rs` | TC-HOOK-INV-1..11 |

## Structural difference — a fold, not an append-time validator

Upstream registers these rules as a cordis companion plugin that refuses a bad
append as it happens. This workspace has no invariant registry, and
`crates/turn/tests/upstream_session_invariants.rs` already records that same
choice for the session store's own rules: *"tetanus has no validator to refuse
an append, so the claim is about the writer."*

So `hook_stream_faults` folds a journal and reports what is wrong with it. The
rules are upstream's, unchanged; only the moment of checking differs, and each
of upstream's `toThrow` cases becomes "this fault is reported".

The consequence, stated rather than left implicit: a fold cannot stop a bad
record being written. This is a conformance check on producers, not a guard
against them. Whether this workspace should grow an append-time validator is a
question for the session store, not for the hooks block, and porting one here
would have invented a registry no other crate has.

Upstream's plugin also carries cordis plumbing — `WeakMap` staging across
`internal/dispatch`, seeding a trace from an existing session, adopting a
session first seen through publication. All of it exists to make an
append-time check work inside that framework. None of it is a rule about hook
events, and none of it is ported.

## Cases beyond the upstream suite

- **TC-HOOK-INV-10** — a duration that is absent or wrong-typed is the same
  fault as a negative one. Upstream pins only the negative case; absent and
  wrong-typed are what a producer is likelier to write, and all three leave the
  trail without the timing it promises.
- **TC-HOOK-INV-11** — every fault is reported, not the first. A producer being
  fixed wants the whole list. Upstream throws on the first because it is
  refusing an append, which is a different job from describing a journal.

## Changelog row

| 2026-08-21 | The hook stream invariants ported (`crates/hooks/src/invariant.rs`, TC-HOOK-INV-1..11): the pairing, enclosure and field rules that make the `hook/*` audit trail trustworthy. A result must answer an invocation that actually happened, at its own point in its own turn - one handler configured at two points is two pairs, and the correlation key is the triple, not the handler id. Both records must sit inside the turn they name. Upstream enforces this as a cordis plugin that refuses a bad append; this workspace has no invariant registry and `upstream_session_invariants.rs` already records that choice for the session store, so the rules are folded over a journal instead and each `toThrow` becomes a reported fault. The consequence is stated rather than implied: a fold checks producers, it does not guard against them, and growing an append-time validator is the session store's question and not the hooks block's. Upstream's `WeakMap` dispatch staging and session-adoption plumbing exists to make an append-time check work inside cordis, is not a rule about hook events, and is not ported. Two cases go beyond upstream: a duration that is absent or wrong-typed is the same fault as a negative one, since those are what a producer is likelier to write, and every fault is reported rather than the first, because a producer being fixed wants the whole list. A mutation check confirms the suite bites: ignoring the turn mismatch, accepting an unpaired result, keying pairs by handler id alone, accepting a negative duration, and accepting any dialect each fail one to three cases. |
