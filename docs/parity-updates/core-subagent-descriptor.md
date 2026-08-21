# Parity update — the subagent descriptor

Slice: `core-subagent-descriptor`. Lane: engine/core (phase ②, subagent block).

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/subagent/subagent/src/descriptor.ts` | `crates/subagent/src/descriptor.rs`, `crates/subagent/tests/descriptor.rs` | TC-SUB-DESC-1..12 |

Upstream has no `descriptor.spec.ts`: these rules are exercised indirectly
through `continuation.spec.ts` and `list-children.spec.ts`, both of which need
the service. Porting the record on its own is what makes them assertable
without one — the same split that worked for the hook bridges' payloads.

## The rule pair worth naming

An **unknown version** reads as absent; an **undeclared field at the current
version** is refused. They look similar and are opposite decisions:

- A newer runtime wrote the child. It is not classifiable here, and "not
  classifiable" is not "broken".
- A record at *this* version with a field this version does not declare is
  corrupt or hand-edited, and reading it would silently ignore composition
  somebody asked for.

That is also why the record snapshots a closed list of named fields instead of
the options object: an unrelated extension's value — possibly not even JSON —
must not be able to make a resume fail.

## The service surfaces this does NOT port, and what they need

Assessed for the payload-versus-wiring split. The record is the payload half
and is complete. The wiring half needs surfaces this engine does not expose:

| Upstream | Needs | Status |
| --- | --- | --- |
| `service.spec.ts` (495 lines) | a provider registry (`ctx.subagents`): register/list/remove, capability negotiation, admission | **not portable yet** — no registry surface |
| `continuation.spec.ts` (2523) | the above, plus a live agent registry (`ctx.agents`) and cold resume constructing a child agent from the descriptor | **not portable yet** |
| `list-children.spec.ts` (1214) | a session store that enumerates journals by parent, plus the agent registry for live children | **partially blocked** — the store enumerates, the live half does not exist |

None of that is blocked on this crate. Each becomes portable when the engine
grows the registry surface, and the descriptor is the durable half every one of
them reads. Standing up a private registry here would be a second competing
surface, the same reason the hook bridges' wiring is deferred.

## Cases beyond the upstream suite

- **TC-SUB-DESC-10** — the *first* record wins. A journal with two descriptors
  is a child re-seeded from another origin, and scanning to the last would let
  a later append rewrite a child's identity.
- **TC-SUB-DESC-11** — a written record reads back identically. The record
  crosses a process boundary, and a resume that could not reconstruct the
  composition would silently start a differently-composed child.
- **TC-SUB-DESC-12** — an absent field is omitted, never written null. A null
  would be refused on the way back in, so writing one produces a record this
  module cannot read. It also pins that a one-shot record never carries
  continuable composition even if the value in hand has some.

## Changelog row

| 2026-08-21 | The subagent descriptor ported (`crates/subagent/src/descriptor.rs`, TC-SUB-DESC-1..12): the durable `subagent/descriptor` record that says which provider established a child and whether it is a one-shot run or a resumable conversation. Upstream has no descriptor suite - its rules are exercised through the service - so porting the record alone is what makes them assertable without one, the same payload-versus-wiring split the hook bridges used. The rule pair worth naming is that an unknown version reads as absent while an undeclared field at the current version is refused: a newer runtime's child is not classifiable here and that is not the same as broken, whereas an extra field at this version is corruption whose quiet acceptance would ignore composition somebody asked for. That is also why the record snapshots a closed list of named fields rather than the options object - an unrelated extension's value must not be able to make a resume fail. Three cases go beyond upstream: the first record wins, since scanning to the last would let a later append rewrite a child's identity; a written record reads back identically, which is what a cold resume depends on; and an absent field is omitted rather than written null, because a null would be refused on the way back in. The service, continuation and list-children suites are assessed and deliberately not ported: each needs a provider or agent registry surface this engine does not expose, and standing up a private one here would be a second competing surface. |
