---
date: 2026-08-20
order: 43
---
A throttled provider's `Retry-After` is read off its answer (`deepseek::retry_after_ms`, TC-DS-WAIT-1..7). The retry policy has honoured a provider-asked wait since it was ported, but nothing ever asked: the transport dropped the header, so every 429 fell back to local backoff and the shortest wait a rate-limited route could take was the one tetanus guessed. Both forms RFC 9110 defines are read; a value that is zero, past or unreadable asks for nothing, because refusing costs one backoff and obeying an uninterpretable value could park a route.
