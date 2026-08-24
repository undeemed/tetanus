---
date: 2026-08-19
order: 15
---
Observer panics are contained (`emit` and `parallel`) and ported (TC-PORT-CONTAIN-1..5, TC-PORT-REG-4). `serial` and `waterfall` stay loud on purpose, so the `core/*` gap now names only the throwing decision listener.
