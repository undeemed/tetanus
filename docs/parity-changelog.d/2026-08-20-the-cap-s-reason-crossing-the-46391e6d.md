---
date: 2026-08-20
order: 56
---
The cap's reason crossing the published boundary is now asserted at the boundary (`crates/engine/tests/max_tokens.rs`, TC-CAP-1 and TC-CAP-2). The turn-level cases pin what the loop does; these ask the same calls a surface asks, so they pin what a caller reads: `agent.prompt` answers `Other("max-tokens")` and the durable `turn/end` carries the same word, the cut-off turn dispatches no `tool/call`, and it still releases the session, because a caller whose answer was cut off has every reason to prompt again. `docs/interface-contract.md` §6 names both clauses.
