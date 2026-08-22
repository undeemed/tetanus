# Parity note: context and durable state

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.
Written here rather than in place because every lane collides on that file.

Branch: `fm/tetanus-p2-ctx`.
Areas: `compaction/*` (12 specs), the persistence and projection half of
`session/*` (38), and `storage/*`, `spill/*`, `credentials/*` (15).

---

## 1. Section 3 rows, as they should now read

### `session/*`, `session-query/*` (38)

**Today** gains: SQLite persistence behind the `SessionLog` seam and a lossless
migration both ways; the projection units themselves - title, stats, token
usage, context pressure and context breakdown - over the existing projection
seam; and the request envelope those units anchor on.

**Gap** keeps: telemetry, log export and query. `session-query/*` stays ③.

Drop from the gap: `SQLite persistence`, `projections and caches`, `titles`,
`stats`, `checkpoints`.

### `compaction/*` (12)

**Today** gains: applying the pruner to a turn's history, the session
transaction that records a prune durably, and model-driven compaction of the
conversation itself - the three things the row named as missing.

**Gap** becomes: the manual `/compact` command, the per-model policy table, and
the surface-changed retries that guard an asynchronous summarizer racing a
concurrently appending session.

The row's whole `Gap` column as written is closed.

### `storage/*`, `spill/*`, `credentials/*` (15)

**Today** gains: the spill policy and its owner-only local backend, and the
credential store - the process environment over an owner-only file, with values
that never enter a config layer.

**Gap** becomes: the SQLite key-value backend and the registry that would choose
between it and the JSON one, and the domain layer over them.

Drop from the gap: `spill policy`, `credential store`.

### `llm/*` (34)

**Gap** drops `the three token projections` and the clause about the request
envelope: `request/context` is on the journal, so the anchor exists.

Note for whoever reconciles: three `llm/*` rows are in the file from union
merges, and each spells its remaining gap slightly differently. All three carry
the token-projection clause.

---

## 2. Section 4 rows

### `llm/token-meter/tests/token-usage-projection.spec.ts`, `context-breakdown-projection.spec.ts`

Was: `none yet` / `unblocked: ... What is left is the three units themselves`.

Now: **ported** to `crates/turn/tests/upstream_projections.rs`, TC-PORT-PROJ-13
through -26.

The three units are `crates/turn/src/projections.rs`; the two that price
nothing are `crates/session/src/units.rs`, because a listing that wants a title
must not have to link a provider adapter. The envelope the usage anchor wanted
is the new `request/context` durable event, written by each step before it
dispatches.

Two differences worth carrying into the row. Upstream's usage arrives twice per
step - a streamed `usage` chunk and the assembled message - so its fold has a
replace-in-place rule for the second report; a tetanus stream carries usage only
on the assembled `assistant/message`, so TC-PORT-PROJ-15 states the same rule
over a repeated report for one step. And a report is identified by the
`step/start` that encloses it rather than by coordinates on the report, because
contract section 4.3.1 gives `assistant/message` no `turn` or `step`.

That second point is a defect the port found: reading coordinates that are not
there made every step of a turn look like a repeat of the first, so a whole turn
was counted once. TC-PORT-PROJ-24 catches it over a real session.

### `compaction/compaction-tool-result-pruner/tests/tool-result-pruner.spec.ts`

Was: `part ported`, with `Its session-transaction half ... needs a durable event
type this contract has not published, so it stays phase ②`.

Now: that clause is closed. The durable type is `compaction/prune`, and the
transaction is `compaction::prune_results`, pinned by TC-PORT-COMPACT-11 and
-12. The rest of the row stands.

### New row: `compaction/compaction/tests/compaction.spec.ts`, `tool-pairing.spec.ts`, `compaction-basic/tests/compaction-basic.spec.ts`

Ports to `crates/turn/tests/upstream_compaction.rs`.

Asserts: which span is compacted and where a cut may fall; the durable
transaction and its adjacency; that the compacted history is what a replay
derives.

part ported: TC-PORT-COMPACT-1..15 for the surface a replacement rewrites, the
position it takes, a record whose replacement never landed, the tool-pairing
boundary, the selected range, a conversation short enough to leave alone, the
transaction's order and adjacency, a compacted session that is smaller and
replays identically, a summary that is not smaller refused and recorded, the
lock a compaction holds, the pruner's session transaction, a turn that compacts
itself and carries on, a turn inside its budget that compacts nothing, and a
budget that cannot converge.

Upstream's manual `/compact` command, its per-model policy table and its
`sourceCommandId` provenance are surfaces tetanus has not built. Its
surface-changed retries guard an asynchronous summarizer racing a concurrently
appending session; a tetanus session has one writer and a turn compacts inside
its own step, so there is no second writer to race. Its `session/end-seed`
boundary has no counterpart - `fork_seq` on the header states the same boundary.

Two decisions are tetanus's own and are worth stating in the row. The
replacement takes the *position* of the range it shadows rather than the end of
the conversation, so a checkpoint of the first twenty messages stands where
those twenty were. And the cheap remedy runs first: pruning tool results needs
no provider and is often enough, so a summary - which costs a call and loses
detail - is asked for only when pruning was not.

The port found one defect of its own: `select_range` guarded an underflow with
`then_some`, whose argument is evaluated eagerly, so a session with nothing to
compact panicked instead of declining. TC-PORT-COMPACT-6 is that case.

### New row: `session/session-persistence-sqlite/tests/sqlite-backend.spec.ts`

Ports to `crates/session/tests/upstream_sqlite.rs` and
`crates/engine/tests/sqlite_backend.rs`.

Asserts: that a second backend answers the way the first one does.

part ported: TC-PORT-STORE-Q1..Q21 for the round trip, the two backends
answering one script identically, durability without a flush, many journals in
one database, a session that exists before its first append, numbering continued
across a reopen, an unrelated database refused, a future schema refused
distinctly, the two seeds refused, migration in and out, a byte-identical round
trip, an export that will not overwrite, the broadcast every append makes, and
then at engine level the same answers over either backend, one artifact rather
than a directory, a restart, a fork seeded into the database, a path refused
where an id is the name, an unserved backend name, and the dump reporting which
backend is running.

Most cases are stated as an equality between what SQLite answers and what JSONL
answers for the same script, because the claim the seam exists for is that a
caller cannot tell them apart.

Upstream's packed chunk rows, revision tokens, write-behind coordinator and
incarnation identity all serve a batching persistence layer tetanus does not
have - every tetanus append is its own commit - so what ports is the schema's
shape and the ownership check on open. Its lazy materialization is inverted
rather than dropped: upstream writes its `sessions` row on the first append to
mirror its JSONL backend's "no file until first append", and the tetanus JSONL
backend creates the file at open, so the row does too and the two agree
(TC-PORT-STORE-Q5).

### New row: `credentials/credentials/tests/credentials.spec.ts`, `credentials-local/tests/local.spec.ts`

Ports to `crates/config/tests/upstream_credentials.rs` and
`crates/engine/tests/credential_containment.rs`.

Asserts: where a secret lives, who may write it, and that it is in no artifact.

part ported: TC-PORT-CRED-1..15 for a stored value resolving, an unconfigured
reference, the environment winning and refusing to be written through, an empty
value being an absent one everywhere, a reference that is not a POSIX
identifier, the owner-only file and a wider one refused, unsetting, a listing
that names references and never values, a value changed on disk reaching the
next operation, a secret that cannot be printed by accident, a malformed
document refused without quoting itself, a credential absent from the settings
document, and then the three containment cases.

Upstream's hot-reload watcher and cross-process write lock are surfaces this
crate does not have; a value is read from the file on every resolve instead,
which gives the same "a changed credential reaches the next operation" property
without a thread, and TC-PORT-CRED-9 pins it. Its `.env` fallbacks in the
invocation directory and in its home are two further read-only layers of the
same kind as the environment; tetanus has one. Its YAML comment preservation has
nothing to restate: the document is JSON and holds nothing but credentials.

TC-PORT-CRED-13 is the acceptance case and it greps rather than asserting on
fields - a real turn runs, then the dump, every byte of every journal under the
sessions root, and the events the boundary serves are searched for the value.
Grep is the right instrument because it is indiscriminate; a typed assertion
only checks the fields someone thought of. Each containment case also proves the
secret is resolvable, so a store that held nothing would fail rather than
satisfy every claim by vacuity.

### New row: `spill/spill-local/tests/spill-local.spec.ts`, `spill-policy/tests/spill-policy.spec.ts`

Ports to `crates/core/tests/spill.rs`.

Asserts: where an oversized payload goes, and what the model reads instead.

part ported: TC-PORT-SPILL-1..9 for a payload within the cap, one over it stored
whole and replaced bounded, a replacement never larger than its cap or its
input across a spread of both, a cap too small for a notice, a preview that
never splits a character, artifacts scoped and named safely, two results of one
call not colliding, a storage failure keeping the inline content, and an
owner-only artifact.

Upstream's policy is a `tools/post-execute` listener composing through `next()`;
tetanus has no post-execute projection seam yet, so the decision and the storage
are published for the pipeline to call and the composition cases have nothing to
restate. Its content-block handling - leaving a result carrying any non-text
block untouched - is unrepresentable: a tetanus tool result carries a `String`.

TC-PORT-SPILL-3 is the case with a defect behind it rather than a rule: a policy
that spends its whole budget on a preview and then appends the notice produces,
for a marginally over-cap input, a replacement *larger than the original*. The
notice is priced at its worst case and reserved before the preview is cut.

---

## 3. Changelog rows

Three rows for [`../parity-changelog.md`](../parity-changelog.md), which is
append-only and `merge=union`. Reproduced verbatim so the reconciliation slice
appends rather than rewrites.

| 2026-08-21 | A second session-persistence backend (`crates/session/src/sqlite.rs`, TC-PORT-STORE-Q1..Q21), opening the persistence half of the `session/*` row. The `SessionLog` trait had been the session seam since phase ① and nothing had ever crossed it: `LiveSession` held a concrete `JsonlSessionLog`, so the abstraction was untested by construction. The engine now holds `Arc<dyn SessionLog>` and `sessions.backend` picks, which is what makes most of the suite an equality between the two backends' answers to one script rather than a second set of assertions. Durability is per append on both - `synchronous = FULL` under WAL fsyncs each commit and each append is its own commit - because a backend a caller cannot tell apart must not quietly promise less than the one it replaced. The database says what it is: an `application_id` and a `user_version` are stamped at creation and checked at open, so an unrelated file is refused rather than grown a `sessions` table, and a future schema is refused distinctly from a stranger's file for the reason the JSON store already gives - one is corrupt, the other may still be running. Migration is lossless both ways to the byte, which TC-PORT-STORE-Q12 pins by comparing the files rather than the events, because both writers serialize the same `SessionEvent`. The backend is resolved at boot rather than at the first `session.create`: a store this build cannot read is a deployment fault, and one that waits for the first turn to report itself is one a user finds first. |
| 2026-08-21 | Context compaction implemented (`crates/turn/src/compaction.rs`, TC-PORT-COMPACT-1..15), closing the `compaction/*` gap. A session that outgrew its window had nowhere to go: the request grew until a provider refused it with `CONTEXT_WINDOW_EXCEEDED`, which is terminal by design, so the turn simply failed. The surface is derived and never stored - nothing is deleted or rewritten to make a span disappear. A `compaction/summary` names the events it shadows, the next surface event replaces them, and `compaction::surface` applies that rule wherever history is derived, so a replay reproduces the compacted history from the same records rather than from a second stored copy that could disagree. The adjacency of a record and its replacement is load-bearing rather than tidy: it is what lets a bounded fold price a replacement with one running total and one pending claim instead of keeping a price per message and growing a checkpoint without bound, and TC-PORT-COMPACT-3 pins that anything between them expires the claim. A cut never splits a tool call from its result, decided over the current surface rather than over step markers, because compaction moves surface positions and step markers do not follow. The cheap remedy runs first: pruning tool results needs no provider and is often enough, so a summary is asked for only when pruning was not. A summary that is not smaller is refused, because committing one leaves the session over budget with a replacement that gets compacted again on the next step for ever. One defect found in the writing: the underflow guard in `select_range` used `then_some`, whose argument is eager, so a session with nothing to compact panicked instead of declining. |
| 2026-08-21 | The five session projections implemented (`crates/turn/src/projections.rs`, `crates/session/src/units.rs`, TC-PORT-PROJ-13..26), closing the two section 4 rows that were marked blocked. The projection seam had existed since TC-PORT-PROJ-1..12 with nothing registered on it. The two that price nothing - title and stats - live in `crates/session`, so a listing that wants a title need not link a provider adapter; the three that price live beside the token meter. Both halves of the split were needed by the same blocker: the request envelope. `request/context` is now written by each step before it dispatches, carrying the route, its context window and what the system prompt and tool catalog cost, and it joins `MOCK_TURN_FLOW` deliberately - after `system-prompt/assemble` because it prices that assembly, before `agent/request` because a listener rewriting the request must not be able to change what the journal already said the request was, and before the answer because a turn a provider failure ended should still say what it tried to send. Two defects found. The usage fold read `turn` and `step` off `assistant/message`, which contract section 4.3.1 does not give it, so every step of a turn looked like a repeat of the first and a whole turn was counted once; a report is now identified by the `step/start` enclosing it, which needs no contract change and is the more robust rule. And TC-CONTRACT-1 asserted that *no* event a turn writes is unparsed, which section 4.3.2 makes impossible to keep while the vocabulary grows - it is now stated over the documented types, with TC-CONTRACT-1b holding the other half, that an unknown type reaches a surface whole. |
| 2026-08-21 | A credential store and a spill policy (`crates/config/src/credentials.rs`, `crates/core/src/spill.rs`, TC-PORT-CRED-1..15 and TC-PORT-SPILL-1..9), closing two thirds of the `storage/*`, `spill/*`, `credentials/*` row. A credential in the settings document is a credential in every artifact - the document is read into layers, published by `config.dump`, quoted in diagnostics and pasted into bug reports - and `crates/config/src/secret.rs` exists precisely to redact values that should never have been there. The store is where they belong: the process environment over an owner-only file, values that never enter a layer at all. The environment wins and is visibly read-only, because a key supplied at launch is this run's explicit intent and nothing in the process can edit it; a write against a reference the environment supplies is refused rather than accepted into a file resolution would then ignore, since a write that appears to succeed while the old value keeps being used is the worst of the three available behaviours. `Secret` is neither `Clone` nor derived-`Debug` and renders as the redaction in both formatting traits, so the one leak that needs no mistake in the store - a struct holding a secret being debug-printed into a log line - produces nothing. TC-PORT-CRED-13 is the acceptance case and it greps a real run's dump, journals and served events rather than asserting on fields, because a typed assertion only checks the fields someone thought of; it also asserts the secret is resolvable, so a store that held nothing cannot pass by vacuity. The spill policy reserves its notice's worst-case cost inside the cap before cutting the preview, for the defect TC-PORT-SPILL-3 sweeps for: spending the whole budget on a preview and then appending a notice makes a replacement larger than the original for a marginally over-cap input, which is the one outcome a size policy must never have. |
