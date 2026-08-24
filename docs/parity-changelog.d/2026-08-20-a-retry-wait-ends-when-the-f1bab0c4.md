---
date: 2026-08-20
order: 35
---
A retry wait ends when the turn is interrupted (`crates/turn/src/interrupt.rs`, TC-INT-1..4, TC-PORT-RETRYX-6..7). Until now a cancel that arrived during a backoff was honoured when the wait ended, so asking a turn to stop could take the whole delay; and a cancel that beat the policy still wrote an `llm/retry` promising an attempt nobody would make.
