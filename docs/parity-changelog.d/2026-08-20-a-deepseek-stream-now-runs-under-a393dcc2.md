---
date: 2026-08-20
order: 53
---
A DeepSeek stream now runs under an idle window (`DEFAULT_STREAM_IDLE_TIMEOUT_MS`, `DeepSeekConfig::stream_idle_timeout_ms`, TC-DS-IDLE-1..4). Until now the adapter set no timeout of any kind, so a provider that accepted a connection and then said nothing was a turn that never ended: no step boundary was reached, so the interrupt could not cut it either. Five minutes of silence is upstream's own figure, and the failure is `TIMEOUT`, which the default retry policy asks again on.
