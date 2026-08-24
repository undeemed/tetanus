---
date: 2026-08-20
order: 38
---
A tool's concurrency classifier is contained (`ToolRegistry::mode`, TC-PORT-MODE-1..8). Until now only a tool's body was contained, so a panic in the classifier - which runs first, before anything the model reads - unwound into the scheduler and took the turn down. It now answers exclusive, which is the answer that overlaps nothing, and the call still runs.
