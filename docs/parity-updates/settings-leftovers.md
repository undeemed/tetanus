# Parity note: the settings leftovers, and scoped stores

Slice: per-namespace schemas, schema-driven redaction, writing the document
back, the watcher wired to the re-read, and the scoped stores the `core/*` row
names.
Branch: `fm/tetanus-p2-settings`.
For folding into [`../parity.md`](../parity.md) by the reconciliation slice;
this lane does not edit the shared file.

These are the gaps left on rows that were already partly served, so what follows
is a set of edits to existing rows rather than a new one.

## 1. `settings/*`, `boot/*` - the gap column, emptied

The row's `Gap` column reads:

> A file watcher to drive the re-read, per-namespace schemas - which is also
> what would let a scalar written where a section belongs be refused rather than
> ignored (TC-PORT-SET-5) - redaction driven by a schema rather than by a name,
> writing the document back

All four are now served. The `Today` column gains:

> per-namespace schemas, so a scalar written where a section belongs is refused
> and a value of the wrong shape is caught as the document is read rather than
> mid-run by whichever reader got to it first; redaction decided by that
> declaration, with the key's name as the fallback for a key no namespace
> claims; the document written back, atomically and owner-only, refusing to
> write through a scalar or over a document it could not parse; and the watcher
> joined to the re-read, so an edit while the harness runs reaches the running
> configuration, a bad edit leaves it standing, and the schema is applied at run
> time as well as at boot

and the `Gap` column becomes:

> Comment-preserving writes, and the revisions and conflict detection of a
> settings *service* that also publishes to subscribers

## 2. `core/*` - one gap closed

`Scoped stores` leaves the gap list. The `Today` column gains:

> scoped stores: working state keyed per scope that one scope cannot read from
> another, disposed by an `EffectHandle` so it dies with whoever opened the
> scope

## 3. Section 4 rows to add

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `settings/settings/tests/settings.spec.ts` (schema half) | `crates/config/tests/schema.rs` | What a namespace declares, what that refuses, and what it still accepts | ported: TC-PORT-SET-5..12, with TC-PORT-SET-5b keeping the unschema'd answer |
| `settings/settings/tests/redact.spec.ts` (schema half) | `crates/config/tests/schema.rs`, `crates/engine/tests/catalog.rs` | Redaction decided by declaration, with the name rule as the fallback | ported: TC-PORT-SET-10 beside TC-SECRET-* and TC-CFG-SECRET-* |
| `settings/settings-file/tests/local.spec.ts` (persist half) | `crates/config/tests/write.rs` | Read-modify-write, what a write refuses, atomic owner-only replace | part ported: TC-PORT-WRITE-1..9 |
| `settings/settings-file/tests/watcher.spec.ts` (drive half) | `crates/config/tests/reload.rs` | A settled edit reaching the running configuration, and what a bad one does not do | ported: TC-PORT-RELOAD-1..8, beside TC-WATCH-* for the settling rule itself |
| `core/scope/tests/scope.spec.ts` (store half) | `crates/core/tests/scoped.rs` | Per-scope state, isolation between scopes, and disposal | ported: TC-PORT-SCOPED-1..8, beside TC-EFFECT-2 and -3 for teardown order |

## 4. What is unrepresentable, and why

- **Comment-preserving writes.** Upstream persists through a YAML editor that
  keeps comments and formatting. Doing that needs a round-tripping parser this
  workspace does not have; `serde_norway` parses to values and renders from
  them. TC-PORT-WRITE-7 pins the loss as behaviour rather than leaving a user to
  discover it, so the day a round-tripping parser lands, the case that changes is
  the one that documented the gap.
- **The settings *service* half of the write path.** Upstream's `update`,
  `mutate` and `publish` carry revisions, detect a conflicting concurrent write,
  and notify subscribers. That is a service over the file, and the pieces it
  would need - a revision counter and a subscription seam - have no other
  caller here. What this slice serves is the file half that such a service
  would sit on.
- **Upstream's schema vocabulary.** Schemastery describes ranges, patterns,
  enums, defaults, nested objects and dictionaries. `schema::Kind` is coarse
  because the questions a settings schema must answer *that nothing else can*
  are the three this slice serves: is this a section, is this the right shape,
  and is this a credential. A range is a check the reader that wants the value
  can make with the value in hand, and putting it here would mean every
  constraint in the harness having a second home.
- **`credential-ref`.** Upstream's schema distinguishes a value that *is* a
  credential from one that *names where a credential lives*. tetanus already
  spells the second as its own key (`api_key_env`), and TC-SECRET-* pins that
  the two are told apart, so the role has nothing to add.
- **An unknown key in a declared namespace.** Upstream refuses it; TC-PORT-SET-9
  pins that tetanus accepts it. A whitelist would make the schema a second
  register every plugin must join before its settings can be written, and the
  first casualty would be a deployment configuring a tool this build does not
  ship. The schema narrows what can go wrong rather than enumerating what may
  exist.
- **Upstream's scope keys and parent chain.** A scoped lookup that falls through
  to an ancestor is how a child ends up acting on a belief its parent
  established - the borrowed-knowledge failure the filesystem observation policy
  exists to stop one layer up - so the chain is deliberately absent rather than
  merely unbuilt.
- **A watcher thread.** `Reload` is a step the caller drives, like the
  `Watcher` beneath it. Upstream's dispose-quiesce case (a watcher stopping
  while a callback is in flight) therefore has nothing to restate: there is
  nothing in flight when a caller stops calling.

## 5. Where a case moved

`TC-PORT-SET-5` asserted that a scalar written where a section belongs is
*ignored*, and named the missing schema as the reason. It now asserts the
refusal, which is what upstream asserts. The old answer survives as
`TC-PORT-SET-5b`, because it is still the answer for a deployment whose keys no
namespace declares, and what mattered about it - that the write must not
*half*-apply - is unchanged either way.

`TC-BOOT-3` grew a second half rather than moving: shape faults are now caught
as the document is read, and value faults still as the settings are resolved.
The case says which stage catches which, because "refused somewhere" is not the
promise a reader needs.
