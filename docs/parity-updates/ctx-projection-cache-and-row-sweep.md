# Parity note: the projection cache, and a sweep of the rows this lane owns

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.

Branch: `fm/tetanus-p6-ctx-rows`.
Method: every clause of the `compaction/*`, `session/*` and `context/*` Gap
columns read against the code that exists today, then each one either built or
written down with what it waits on.

---

## 1. Built here: the durable half of the projection seam

`crates/session/src/projection_cache.rs`, TC-PORT-PCACHE-1..6.

`Projections::checkpoint` and `Projections::restore` have existed since the
projection seam landed and **nothing ever wrote one down**: the two were
exercised only by their own unit cases, so every reader folded from seq zero on
every open - the exact cost the checkpoint was built to avoid. This is the
fourth "the mechanism exists and nothing drives it" finding in these areas, and
the cheapest to miss, because unlike a missing journal field it makes nothing
wrong, only slow.

The rules are upstream's and they are all about not trusting the row: it is a
shortcut and never an authority, possibly stale but never wrong, a version
mismatch discards rather than migrates, and every path is fail-soft - an
unreadable cache is an empty cache and an unwritable one is a longer replay,
because a session that would not open because its *cache* was corrupt would be
a session lost to an optimisation.

One case is shaped by that: TC-PORT-PCACHE-1 asserts the number of events
folded, not the value produced. A reader with no cache at all produces the
identical value, so a case comparing values would pass with the cache deleted.

It was buildable now and not before because it needs a key-value store, and
which medium that is has to be the deployment's choice - the seam that landed
in the commit before this one.

## 2. Rows that are stale rather than open

Section 3 still carries these as Gaps. Each is served, with the case ids:

| Row | Gap clause | Where it is served |
| --- | --- | --- |
| `compaction/*` | "Applying it to a turn's history" | `TurnEngine::fit_context`, `crates/turn/src/compaction.rs` |
| `compaction/*` | "the session transaction that records a prune durably" | `compaction/prune`, bracketed by `compaction/start` and `compaction/end` |
| `compaction/*` | "model-driven compaction of the conversation itself" | the awaited summariser behind `compaction/summary` |
| `session/*` | "SQLite persistence" | `crates/session/src/sqlite.rs`, behind the `SessionLog` seam |
| `session/*` | "the projections themselves (usage, pressure, breakdown, stats, title)" | `crates/turn/src/projections.rs` and `crates/session/src/units.rs` |
| `storage/*` | "The SQLite backend and the registry" | the commit before this one |
| `context/*` | "Time ... context" | the commit before that |

Suggested rewrite of the `compaction/*` Gap, which is then empty of anything
buildable: `upstream's manual /compact command and its per-model policy table,
both of which are surface rather than parity`.

Suggested rewrite of the `session/*` Gap: `telemetry, and log export and query
(3)`. Both notes below.

## 3. What is genuinely left, and what each waits on

- **Session telemetry** (`session/*`). Upstream's `session-telemetry` and
  `session-telemetry-otel` are two packages, and the second names the reason
  this is not a port: it is an OpenTelemetry integration. The decision that
  gates it is a dependency decision - whether this workspace takes
  `opentelemetry` and its transitive tree - and that belongs to whoever owns
  the dependency policy, not to a parity slice. The shape underneath it is
  already here: the projection seam is the fold, and a telemetry unit is a
  projection that exports instead of serving a view.
- **Log export and query** (`session-query/*`). Already marked ③ in the row's
  own phase column. Half of export exists as a side effect of the second
  session backend: `import_jsonl` and `export_jsonl` are a lossless round trip
  between the two media. What ③ means beyond that is a query language over a
  journal, which is a feature and not a port of anything that exists.
- **A manual `/compact`** (`compaction/*`). Upstream's is a slash command in
  its surface. tetanus has no slash commands, and inventing one to host a
  single verb is a surface decision for the presentation lane; the engine half
  it would call is served.
- **Per-model compaction policy** (`compaction/*`). Upstream keeps a table of
  context windows per model. tetanus reads the window from settings, which is
  the same fact configured rather than compiled in, so what is missing is a
  shipped default table - data, and stale data at that, since models change
  faster than a release.
- **tmux context**, **instruction re-render**, **the guards**: unchanged from
  `ctx-runtime-context.md` section 4.

## 4. Changelog row

| 2026-08-22 | The durable half of the projection seam (`crates/session/src/projection_cache.rs`, TC-PORT-PCACHE-1..6), and a sweep that found most of what the `compaction/*` and `session/*` Gap columns still claim is already served. `Projections::checkpoint` and `restore` had existed since the seam landed with nothing writing one down, so every reader folded from seq zero on every open - the cost the checkpoint exists to avoid. That is the fourth mechanism-with-no-driver found in these areas and the easiest to miss, because it makes nothing wrong, only slow: a case comparing values passes with the cache deleted, which is why TC-PORT-PCACHE-1 asserts the number of events folded instead. Every rule here is about not trusting the row - a shortcut and never an authority, stale but never wrong, a version mismatch discarding rather than migrating, and every path fail-soft, because a session that would not open because its cache was corrupt would be a session lost to an optimisation. It was buildable now and not before because it needs a key-value store whose medium is the deployment's choice, which landed in the commit before it. The sweep half: `compaction/*`'s three Gap clauses are all served (history fitting, the durable prune transaction, the model-driven summariser), `session/*`'s SQLite persistence and all five projections are served, and what remains in those rows is telemetry - an OpenTelemetry dependency decision rather than a port - export and query, which the row already marks ③, a manual `/compact` that is a surface decision, and a per-model window table that is data models outdate faster than releases. |
