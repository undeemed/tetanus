---
date: 2026-08-21
order: 60
---
The durability barrier and session-event containment ported (`crates/turn/tests/upstream_scoped.rs`, TC-PORT-SCOPE-1..6), opening `scoped.spec.ts` for `core/session` in section 4 and closing it in the same pass. `TurnEngine::flush` has dispatched `session/flush` since phase ① and nothing asserted any of it: not that every participant is awaited, not that a panicking one leaves the rest to run, not that what a participant appends is in the journal when the caller continues. A mutation check both confirms the suite bites and bounds what it claims: dropping the dispatch fails three of the six cases, while moving the engine's own `log.flush()` before the dispatch fails none - `JsonlSessionLog` fsyncs each record as it appends it, so the trailing sync commits nothing the appends did not, and the claim with teeth is attendance. The sync stays, because `SessionLog` is a seam and a batching implementation would need it.
