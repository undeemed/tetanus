---
date: 2026-08-20
order: 47
---
The agent loop's scheduling ported as properties (`crates/engine/tests/properties.rs`, TC-PROP-AGENT-1..3), the second `properties.spec.ts` this workspace has an answer for. A burst is raced on a multi-threaded runtime rather than interleaved on one, so how many of it the engine accepts varies between runs and every assertion holds for whichever split happened. No engine change: it found nothing, which is the expected result for a rule three example cases already pin.
