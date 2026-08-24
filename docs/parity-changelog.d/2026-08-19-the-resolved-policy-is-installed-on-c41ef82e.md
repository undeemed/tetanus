---
date: 2026-08-19
order: 26
---
The resolved policy is installed on every session route (`crates/engine/src/agent.rs`, TC-RETRY-6 and -7), so a document is now the only thing that decides whether a failed request is tried again. Until this, `llm.retry` parsed, reported provenance and changed nothing. The executor is scoped to the route the session named, which is where a per-provider block will hook when the document grows one.
