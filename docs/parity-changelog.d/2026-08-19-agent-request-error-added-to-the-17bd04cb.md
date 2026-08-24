---
date: 2026-08-19
order: 22
---
`agent/request-error` added to the turn: a failed model request is offered to a listener before it ends the turn (TC-RECOVER-1..3). It is the seam upstream's `llm-retry` package hooks; tetanus has no listener for it yet.
