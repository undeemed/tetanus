---
date: 2026-08-20
order: 55
---
The two `tool JSON parse` cases ported from `coverage-edges.spec.ts` (TC-PORT-ARGS-1..4). No behaviour changed: arguments that are not JSON already arrived as the text the model wrote, and an empty arguments string already read as no arguments. Neither had a case, so either could have been repaired away by a later change without a test noticing.
