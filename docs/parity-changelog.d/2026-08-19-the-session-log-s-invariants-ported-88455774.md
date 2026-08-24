---
date: 2026-08-19
order: 33
---
The session log's invariants ported as properties (`crates/turn/tests/properties.rs`, TC-PROP-SESS-1..5), the first `properties.spec.ts` this workspace has an answer for. `proptest` joins the workspace as a dev-dependency. A mutation check confirms the suite bites: dropping the empty-assistant-message rule from `derive_messages` fails TC-PROP-SESS-3 and -5 and shrinks to the one-event counterexample.
