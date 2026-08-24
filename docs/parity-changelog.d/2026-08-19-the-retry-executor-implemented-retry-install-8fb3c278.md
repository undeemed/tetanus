---
date: 2026-08-19
order: 25
---
The retry executor implemented (`retry::install`, on the `agent/request-error` recovery point) and ported (TC-PORT-RETRYX-1..5). Each scheduled retry is durable before its wait, so the attempt count is read back from the journal. Settings resolution is what is left of the `retry.spec.ts` row.
