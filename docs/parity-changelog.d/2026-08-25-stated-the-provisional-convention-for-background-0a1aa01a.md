---
date: 2026-08-25
---
Stated the provisional convention for backgrounded output as a contract clause (docs/interface-contract.md section 4.3.6): a backgrounded one-shot command puts its rendered result in the job record's own output field, its unbounded stream in the session's spill artifact, and the artifact's path on the record's detail as JSON. No code and no types: the process lane is about to become the job store's first consumer, and the convention is published before the implementation so the workflow/schedule/jobs row inherits a choice it can see rather than one buried in a call site. It is marked provisional because that row's tool-jobs does not exist yet and may ratify or replace it.
