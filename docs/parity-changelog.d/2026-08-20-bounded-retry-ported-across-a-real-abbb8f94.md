---
date: 2026-08-20
order: 51
---
Bounded retry ported across a real HTTP/SSE boundary (TC-PORT-XPORT-1..5): the adapter, the decoder, the recovery point and the executor all run against a scripted loopback endpoint. It found one gap: no request or idle timeout, so a stalled provider is a turn that never ends.
