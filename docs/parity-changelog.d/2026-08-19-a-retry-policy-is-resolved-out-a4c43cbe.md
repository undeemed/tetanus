---
date: 2026-08-19
order: 24
---
A retry policy is resolved out of the settings document (`crates/engine/src/retry.rs`, TC-RETRY-1..5): upstream's keys under `llm.retry`, upstream's rules, and every refused value naming the key that holds it. The six keys are published in the defaults layer, so `config.dump` shows a policy nobody configured. Installing the resolved policy on a route is the next step; upstream reads it from each provider's own configuration block, and tetanus's document has no per-provider section yet.
