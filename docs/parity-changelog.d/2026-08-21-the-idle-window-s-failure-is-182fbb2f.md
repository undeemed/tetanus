---
date: 2026-08-21
order: 58
---
The idle window's failure is now driven to recovery end to end (TC-PORT-XPORT-6, closing upstream's stalled-body case in `transport-recovery.spec.ts`). TC-DS-IDLE pins the timeout on the adapter alone; this drives the real adapter, decoder, recovery point and retry executor together against a loopback endpoint that sends the head and then goes silent, so a stall becomes `TIMEOUT`, the retry re-sends the same request, and the turn recovers. The `transport-recovery` row's only remaining gap is upstream's whole-request deadline, which the idle window does not cover.
