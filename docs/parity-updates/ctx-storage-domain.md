# Parity note: the domain layer over the two media

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.

Branch: `fm/tetanus-p7-ctx-domain`.
Scope: the `storage/*` row's last Gap clause, "the domain layer over them".

---

## 1. What was built

`crates/core/src/storage/domain.rs`, TC-PORT-DOM-1..12: declared tables with a
validator, an optional global slot, a version stamped on the medium, a change
event per durable write, and the routing that says which store serves which
domain.

`KvStore` is deliberately dumb - tables of JSON with no opinion about what is
in them - which is the right seam for a medium and the wrong surface for a
component, which wants to say "this is my data, this is its shape, tell me when
it changes". This is that surface, and none of it knows which medium answered.

## 2. Decisions worth recording

- **Validation runs in both directions.** A value the declaration refuses is
  never written, and a stored value that no longer validates is *reported*
  rather than served. The second half is the one that matters: a component
  reading a record it would have refused to write is acting on data it does not
  understand, which is what happens the first time two builds share a store.
- **A version refuses at open rather than migrating.** Converting a record
  whose meaning changed is guessing, and the guess is silent.
- **A change is announced after it is durable, carrying the new value only.** A
  consumer that wants a diff keeps its own previous copy; shipping both doubles
  what an event costs every consumer that does not. A refused write announces
  nothing, and a delete that found nothing announces nothing.
- **Routing resolves at open.** A route naming a store nobody mounted fails
  there, because the alternative is a deployment that boots, runs, and fails at
  the first write - by which time it has told a user the thing worked.
- **Tables are namespaced by domain,** because one store holds several
  components and two of them may each have a `state` table without meaning the
  same thing.
- **No schema language.** Upstream validates with zod and later projects the
  same schemas to RPC. This workspace has none at this layer, so a table
  carries a predicate its owner wrote - the same decision `crates/turn`'s tool
  schemas make one layer up. If a schema language ever lands, this is one of
  its consumers.

One thing the implementation learned from the medium: the reserved table was
first spelled `@meta` so a collision with a declared table was unrepresentable,
and the medium refused the name, because that character set is deliberately
safe as a file name, a JSON key and a SQL identifier at once. The collision is
prevented by a rule enforced at open instead (TC-PORT-DOM-12).

## 3. Row edit, section 3

**`storage/*`, `spill/*`, `credentials/*`.** Gap: remove `the domain layer over
them` - the row's Gap column is then empty for this lane's areas. Today: add
`a domain layer over both media: declared tables with a validator, a version
stamped on the medium, a change event per durable write, and per-domain
routing resolved at open`.

## 4. Changelog row

| 2026-08-24 | The domain layer over the two storage media (`crates/core/src/storage/domain.rs`, TC-PORT-DOM-1..12), which closes the `storage/*` row's last Gap clause. `KvStore` is deliberately dumb - tables of JSON with no opinion about their contents - and that is the right seam for a medium and the wrong surface for a component, which wants to declare its shape and be told when it changes. Records are validated in both directions, and the read half is the one that matters: a value the declaration refuses is never written, and a stored value that no longer validates is reported rather than served, because a component reading a record it would have refused to write is acting on data it does not understand - which is what happens the first time two builds share a store. A version is stamped on the medium and refuses at open rather than migrating, since converting a record whose meaning changed is guessing and the guess is silent. A change is announced once, strictly after the write is durable, carrying the new value and never the old one: a consumer wanting a diff keeps its own copy, and shipping both doubles what an event costs everyone who does not. A refused write and a delete that found nothing both announce nothing. Routing is a deployment's - a default store plus per-domain overrides - and resolves at open, because a route that fails at the first write fails after a user has been told the thing worked. Tables are namespaced by domain, since one store holds several components and two of them may each have a `state` table meaning different things. Upstream validates with zod and projects the same schemas to RPC later; this workspace has no schema language at this layer, so a table carries a predicate its owner wrote, as tool schemas do one layer up. The reserved bookkeeping table was first spelled `@meta` so a collision was unrepresentable, and the medium refused the name - its character set is deliberately safe as a file name, a JSON key and a SQL identifier at once - so the collision is prevented by a rule enforced at open instead. |
