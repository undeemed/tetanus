# Parity note: what a stopping server owes its journals

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.

Branch: `fm/tetanus-p9-shutdown`.
Scope: contract section 4.4.11, and the third stop reason found published with
nothing producing it.

---

## 1. What was built

`HarnessEngine::drain`, `Interrupt::stop_because`, `StopReason::Shutdown`, and
TC-SHUTDOWN-1..5.

The filesystem lane's sweep found this one and named it precisely: beyond
`"timed-out"` and `"repeated"`, a third reason - `"shutdown"` - had no
producer, its only textual hits being the protocol's own vocabulary. That was
right. §4.4.11 is a section of the contract, §6 lists TC-PROTO-66 against it,
TC-PROTO-65 and -66 assert the word at the boundary, and both build the value
by hand. No journal tetanus ever wrote carried one.

## 2. The decisions

- **One mechanism, two facts.** The drain interrupts turns through the switch
  `agent.interrupt` already throws rather than a second one, exactly as the
  section requires. What differs is the reason recorded, and the section spends
  a paragraph on why: a transcript that says `"cancelled"` for a rolling
  restart sends its reader after a user who did nothing.
- **The first reason stands.** A drain arriving after a user pressed stop does
  not relabel that turn: overwriting would credit a deployment's restart with a
  decision a person made (TC-SHUTDOWN-3).
- **Bounded and best effort.** A tool that will not return cannot be waited for
  indefinitely, so the drain answers how many turns it could not close, which
  is what lets a process choose between exiting and waiting longer - and what
  makes `"interrupted"` after a restart mean "the drain did not finish"
  (TC-SHUTDOWN-5).
- **An idle drain returns at once** rather than waiting out its budget, because
  a server stopped between turns is the ordinary restart (TC-SHUTDOWN-4).
- **Polled, not notified.** A turn ends by returning from `run_turn` on a task
  the engine does not own, so there is nothing to await; the drain polls the
  busy flag on a short interval.

## 3. What is deliberately left to the presentation lane

Refusing new work while draining, and choosing the signal to drain on, are
`crates/cli`, which the contract's file-ownership table gives to that lane.
The section is explicit that there is **no** "server is stopping" error code -
a stopping carrier closes the connection instead, because adding a code is a
change both lanes land together. The engine half is this; the `tetanus serve`
half is theirs, and it is one call.

## 4. Row edits

**Section 3, `core/*`** (or wherever the reconciliation slice keeps the serve
row): Today gains `a drain that closes running turns on the way out, so a clean
exit leaves nothing for crash repair`. Gap gains `the carrier half of the
drain: stopping accepting connections, and the signal to drain on`.

**Section 6 of the contract** already lists TC-PROTO-66 for §4.4.11's boundary
half; the engine half is TC-SHUTDOWN-1..5 and should be listed beside it when
the contract's verification table is next folded.

## 5. Changelog row

| 2026-08-25 | What a stopping server owes its journals (`HarnessEngine::drain`, TC-SHUTDOWN-1..5), closing the third stop reason found published with no producer - after `"timed-out"` and `"repeated"`, and found by the filesystem lane's sweep rather than by this one. Contract section 4.4.11 is a section, TC-PROTO-65 and -66 assert `"shutdown"` at the boundary, and both build the value by hand: no journal tetanus wrote had ever carried one, so every deploy left half-turns for crash repair to synthesize on the next open. The drain interrupts each running turn at the next step boundary through the switch `agent.interrupt` already throws rather than a second mechanism, and waits for them to close, so a clean exit leaves repair nothing to do. The reason is `"shutdown"` and deliberately not `"cancelled"`: the same event to the engine and different facts to a reader, since one is a decision to respect and the other is a restart to go and look at. A drain that arrives after a user already pressed stop does not relabel that turn, because overwriting would credit a deployment's restart with a person's decision. It is bounded and best-effort - a tool that will not return cannot be waited for - and answers how many turns it could not close, which is what lets a process choose between exiting and waiting longer and what makes `"interrupted"` after a restart mean the drain did not finish. An idle drain returns at once rather than waiting out its budget, since a server stopped between turns is the ordinary restart. The wait polls a busy flag rather than awaiting a notification, because a turn ends by returning on a task the engine does not own. What stays with the presentation lane is the carrier half: refusing new work while draining and choosing the signal, which section 4.4.11 pairs with its own rule that there is no "server is stopping" code - a stopping carrier closes the connection, because adding a code is a change both lanes land together. |
