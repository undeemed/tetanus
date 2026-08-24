---
date: 2026-08-20
order: 40
---
The retry chain's journal invariants ported (`crates/turn/tests/upstream_retry_invariants.rs`, TC-PORT-RETRYINV-1..5). They pin what `retry::install` promised and nothing asserted end to end: a step and a turn each open their own chain, a scheduled attempt is announced once and only after it was promised, no retry opens a step, and the bound is read back off the journal, so a session resumed mid-chain does not get its retries again.
