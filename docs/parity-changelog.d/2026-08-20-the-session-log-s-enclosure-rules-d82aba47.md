---
date: 2026-08-20
order: 52
---
The session log's enclosure rules ported (`crates/turn/tests/upstream_session_invariants.rs`, TC-PORT-SESSINV-1..5). Upstream enforces them in the validator every append passes through; tetanus has none, so one fold states each rule once and five differently shaped runs - plain, three turns, a contained tool panic, a provider failure, an interrupt - are checked against it. Until now the exact sequence of one mock turn was asserted (`turn_flow.rs`) and the shape of a journal nothing wrote was asserted (`properties.rs`), but no case said that whatever the turn did, the journal it left closes every turn and step it opened.
