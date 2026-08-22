# Parity note: the second storage medium, and the registry over the two

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.
Written here rather than in place because every branch in flight collides on
that file.

Branch: `fm/tetanus-p6-ctx-rows`.
Scope: the `storage/*` row's "the SQLite backend and the registry that would
choose between the two". This is the item the earlier sweep of this lane parked
behind a `Cargo.toml` collision with the session backend; that landed long ago,
so the collision is gone.

---

## 1. What was built

`crates/core/src/storage/` is now a seam with two media under it:
`KvStore` (the vocabulary), `json::Store` (the file, unchanged in behaviour
except for one defect below), `sqlite::SqliteStore` (the database), and
`registry::StorageRegistry` (named mounts).

The registry is worth having now and was not before: over one backend it is a
map with one entry and a lookup that cannot fail usefully. Upstream's rule that
several backends stay mounted side by side, and that which one serves which
consumer is that consumer's configuration rather than a hub-wide current
backend, is kept - a global choice cannot be scoped to one component.

Cases: TC-PORT-STORE-C1..C8 are the conformance suite, each case run against
**both** backends; TC-PORT-STORE-S1..S2 are the two the database has and the
file cannot (a foreign database, a future schema); TC-STORE-REG-1..3 are the
registry, including upstream's rule that a stale disposer must not unmount a
successor registered under the same name.

## 2. The defect the shared suite found

The file backend materialized its file on a `remove` that found nothing. Its
own module documentation opens with "nothing is written until something is
stored", and a run whose only storage call was a defensive `remove` left a
store behind. It had passed every case in `storage.rs` for as long as that
backend has existed, because a suite written against one backend tends to
assert what that backend does.

That is the argument for the shape of the new suite, and it is upstream's
argument too (`packages/storage/tests/contract.ts` runs one suite against every
backend): a rule that holds for one medium and not the other is not a rule a
caller holding a `dyn KvStore` can rely on. TC-PORT-STORE-C5 is the case; the
fix is three lines in `json::Store::remove`.

## 3. Row edits, section 3

**`storage/*`, `spill/*`, `credentials/*`.** Gap: remove `The SQLite backend
and the registry that would choose between the two`, leaving `the domain layer
over them`. Today: add `a second medium behind the same seam - one SQLite
database, opened lazily so an unwritten store still leaves no file, stamped
with its own application id so an unrelated database is refused rather than
grown a table - and a named registry that mounts both side by side, with the
rules asserted against both backends by one conformance suite`.

## 4. Row edits, section 4

| Row | Edit |
| --- | --- |
| `storage/storage-json/tests/json-backend.spec.ts` | Add: "Its rules are now also asserted against the second backend (TC-PORT-STORE-C1..C8), which found one defect the single-backend suite could not: a `remove` that found nothing materialized the file, against this module's own 'nothing is written until something is stored'." |
| `storage/storage-sqlite/tests/unit.spec.ts` | New row: restated as TC-PORT-STORE-C1..C8 (the shared rules) and TC-PORT-STORE-S1..S2 (identity stamp, schema version) in `crates/core/tests/storage_backends.rs`. Upstream's write-behind batching has nothing to restate - tetanus commits each write, and `synchronous = FULL` under WAL is what keeps the database's durability promise identical to the file's fsynced rename. |
| `storage/storage/tests/registry.spec.ts` | New row: restated as TC-STORE-REG-1..3. Upstream's facet negotiation (a backend that omits a data kind) has no counterpart: there is one facet, so a backend either implements `KvStore` or is not a backend. |

## 5. What is left in this row

The domain layer over the two media - upstream's typed unit specs, its
migrations and its change events - is untouched and is the row's remaining
Gap. It is a layer over this seam rather than a change to it, which is why it
is nameable now: what it needs from below is exactly what landed here.

## 6. Changelog row

| 2026-08-22 | A second storage medium behind the same seam and the registry that mounts both (`crates/core/src/storage/`, TC-PORT-STORE-C1..C8, TC-PORT-STORE-S1..S2, TC-STORE-REG-1..3), which the earlier sweep of this lane had parked behind a `Cargo.toml` collision that has since landed. The database is opened lazily, because the file backend promises that a store nobody writes to leaves no trace and a backend a caller cannot tell apart must not quietly promise less; it stamps `application_id` and `user_version` at creation, so an unrelated SQLite file is refused rather than grown a `records` table, and the identity is checked before the version, since a coincidental `user_version` on somebody else's database would otherwise be reported as a version problem and send the reader after the wrong answer. `synchronous = FULL` under WAL is the deliberately slower pragma, for the same reason: the file backend fsyncs a rename per write. The suite is the interesting half. Upstream runs one conformance suite against every backend and the reason is what a seam is for, so each of the eight shared cases here runs its whole body against both media and says which one failed - and it immediately found a defect no single-backend suite could: `json::Store::remove` published the file even when the key was not there, so a run whose only storage call was a defensive remove left a store behind, against that module's own opening rule that nothing is written until something is stored. The registry keeps upstream's two rules that matter: a name mounted twice is refused rather than replaced, because silently keeping the second gives half a deployment's consumers the other medium with nothing in a log to say why, and a stale unmount handle must not remove a successor registered under the same name, which is the failure that only appears under a reload. What is left in the row is the domain layer over both media, which is a layer above this seam rather than a change to it. |
