# Parity update — detached hook runs

Slice: `core-hook-detached`. Lane: engine/core (phase ②, hooks block).

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/hooks/hook-protocol/src/detached.ts`, `tests/detached.spec.ts` | `crates/hooks/src/detached.rs`, `crates/hooks/tests/detached.rs` | TC-HOOK-DET-1..8 |

## Structural difference — a cancel signal this crate owns

Upstream uses `AbortController`/`AbortSignal`, which the platform provides and
its shell executor already accepts. There is no equivalent here, so
`CancelSignal` is a small owned primitive: one flag, one reason, one wakeup.

It is deliberately not a dependency on a cancellation crate. The whole of what
detached runs need is "has it fired, why, and wake me when it does", and the
seam this crate hands to an executor should not force a crate choice on
whoever implements it.

## Cases beyond the upstream suite

- **TC-HOOK-DET-6** — a run holding the signal actually observes the
  cancellation. Without it, every other case here would pass against a signal
  nobody consults, which is the failure that leaves a hook process alive after
  shutdown.
- **TC-HOOK-DET-7** — a signal fired twice keeps the first reason. The first
  cause is the diagnosis; a later one is usually its consequence.
- **TC-HOOK-DET-8** — waiting on an already-fired signal returns at once. A
  hook that checks for cancellation after a drain has run must not park on a
  notification that has been and gone.

## A case that passed for the wrong reason, and how it was found

TC-HOOK-DET-4 — "a drain waits for a run tracked while a prior wave was
settling" — **initially passed against a single-wave drain**, which is exactly
the bug it exists to catch. The mutation check found it.

The cause was ordering, not logic. On a current-thread runtime the first run
was polled before the drain task, so it had already tracked the late run by the
time the drain took its first wave: the wave contained both handles and the
late-tracking path was never exercised. A probe confirmed it directly
(`wave=2`).

The case now sleeps *after* spawning the drain and *before* releasing the first
run, so the drain is parked on the first wave when the late run is tracked.
With that ordering the single-wave mutation fails, as it should. The comment in
the case records why the sleep is where it is, because moving it back is a
silent regression to a test that cannot fail.

## Timing discipline in this suite

The two kinds of wait are deliberately different constants, because they fail
for opposite reasons. `A_MOMENT` (50ms) is only ever used for "it should not
have finished yet" — a loaded machine makes that check *more* reliable, since
everything is slower. `PATIENCE` (10s) bounds every "it should finish" — that
one fails when the machine is busy rather than when the code is wrong, and six
lanes share this machine.

## Named test gap — the cancel/wait race

`CancelSignal::cancelled` registers its `Notified` future *before* re-checking
the flag, which is what closes the window where a cancel landing between the
check and the wait would park the waiter forever. **No case pins this**, and
the mutation that opens the window — checking the flag, then awaiting a freshly
created `Notified` — passes the suite.

The window is two statements wide, and hitting it deterministically needs the
task suspended between them, which this workspace has no way to arrange. A
stress loop would be flaky rather than a test. Recorded here instead, in the
same spirit as the constant-time-compare and held-event-de-dup gaps the
`rpc-ws-auth` slice named: the correct implementation is kept and the reason it
cannot be pinned is written down.

## Changelog row

| 2026-08-21 | Detached hook runs ported (`crates/hooks/src/detached.rs`, TC-HOOK-DET-1..8), completing `hook-protocol`. Some hook points are fired and forgotten, and those runs still own a process and still have a continuation that appends to the journal - so disposing an adapter mid-run risks a process outliving the harness, a late append into a closing journal, or a shutdown that blocks for the hook's full ten-minute timeout. The tracker answers all three by cancelling before it waits: a still-running hook is killed rather than waited out, and the wait is for continuations. The drain re-checks in waves rather than waiting on one snapshot, because a run's continuation can track another run while the first wave settles. `CancelSignal` is a small owned primitive rather than a crate dependency, since the seam handed to an executor should not force a crate choice on whoever implements it. Three cases go beyond upstream: a run actually observes the cancellation, without which the whole module could be a signal nobody consults; a signal fired twice keeps the first reason, since the first cause is the diagnosis and a later one its consequence; and waiting on an already-fired signal returns at once. The mutation check earned its place here - TC-HOOK-DET-4 initially passed against a single-wave drain, because on a current-thread runtime the first run completed before the drain took its wave, so the late-tracking path was never reached; the case now orders the sleep so the drain is parked when the late run is tracked. One named test gap: the two-statement window in `cancelled` between checking the flag and awaiting the notification cannot be pinned deterministically, and is recorded rather than faked. |
