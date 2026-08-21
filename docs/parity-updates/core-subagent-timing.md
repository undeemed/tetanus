# Parity update — child timing projection

Slice: `core-subagent-timing`. Lane: engine/core (phase ②, subagent block).

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/subagent/subagent/src/projection.ts` (`subagentTimingProjectionDefinition`), `tests/timing-projection.spec.ts` | `crates/subagent/src/timing.rs`, `crates/subagent/tests/timing.rs` | TC-SUB-TIME-1..10 |

**This is an integration, not a new seam.** `tetanus_session::projection::Projection`
already exists — `key`, `state_version`, `init`, `apply`, `view`, the same shape
as upstream's definition — and this is the first unit in the workspace to
implement it outside the session crate's own tests. The projection slice landed
in the drain said "nothing registers a projection yet"; this is the first that
does.

## The two rules worth the module

- **The descriptor is the origin.** A child's journal can open with inherited
  history, and those turns are the *parent's* work. Reaching
  `subagent/descriptor` resets settled time to zero, or a forked child reports
  its parent's elapsed time as its own from the moment it starts. The one thing
  the reset keeps is a turn already open, timed from its original start — that
  turn is what created the child, so it is the child's first turn.
- **Journal time is not monotonic.** Timestamps are wall-clock, written across
  processes and possibly across a clock adjustment, so a turn can appear to end
  before it began. Such a turn contributes zero, never a negative: a running
  total that shrank would make a reader distrust every number on the page.

## Cases beyond the upstream suite

- **TC-SUB-TIME-6** — a turn open at the descriptor is timed from its own
  start. It is the only reason the fold carries a pending start at all, and
  upstream's suite never isolates it.
- **TC-SUB-TIME-7** — the open window advances with later records, and does not
  reopen after the turn closes. Without the advance a long turn looks frozen.
- **TC-SUB-TIME-8** — a second descriptor resets again. TC-SUB-TIME-2 relies on
  this without saying so.
- **TC-SUB-TIME-9** — the state round-trips, and folding on from the reloaded
  value equals never having stopped. This is what makes it a projection rather
  than a function: a state that did not deserialize into itself would make a
  persisted checkpoint fold on from the wrong value.
- **TC-SUB-TIME-10** — an absurd timestamp saturates. Times come off a journal
  another process wrote, and a wrapped total would report a child that ran for
  eons as one that had barely started.

## Changelog row

| 2026-08-21 | The child timing projection ported (`crates/subagent/src/timing.rs`, TC-SUB-TIME-1..10), and it is the first unit in this workspace to implement the `Projection` seam outside the session crate's own tests - the drain's projection slice noted that nothing registered one yet. A parent showing a child's elapsed time needs a number that survives a resume, and the journal is the only thing that does. Two rules carry it. The `subagent/descriptor` record is the child's origin, so reaching it resets settled time to zero: a child's journal can open with inherited history whose turns are the parent's work, and without the reset a forked child reports its parent's elapsed time as its own from the moment it starts. The reset keeps a turn that is already open, timed from its original start, because that turn is what created the child. And journal time is not monotonic - timestamps are wall-clock, written across processes and possibly across a clock adjustment - so a turn that appears to end before it began contributes zero rather than a negative, since a running total that shrank would make a reader distrust every number on the page. Five cases go beyond upstream, including that the state round-trips and folding on from the reloaded value equals never having stopped, which is what makes this a projection rather than a function. |
