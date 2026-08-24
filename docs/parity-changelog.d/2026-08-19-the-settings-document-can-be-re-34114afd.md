---
date: 2026-08-19
order: 19
---
The settings document can be re-read at run time (`crates/config/src/recompose.rs`) and the runtime half of `watcher.spec.ts` ported (TC-PORT-CFG-11..16). A bad edit keeps the last good configuration. The watcher that would drive it is the remaining `settings/*` gap.
