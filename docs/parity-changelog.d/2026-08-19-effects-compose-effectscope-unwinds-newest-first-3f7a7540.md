---
date: 2026-08-19
order: 10
---
Effects compose: `EffectScope` unwinds newest first, nests, and finishes past a panicking undo, and `Registry::start_all` rolls a failed mount back (TC-EFFECT-1..6, TC-PLUGIN-1..2).
