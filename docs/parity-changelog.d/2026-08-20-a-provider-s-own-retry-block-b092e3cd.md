---
date: 2026-08-20
order: 50
---
A provider's own retry block is installed on its route (`EngineConfig::provider_retry`, `Runtime::policy_for`, TC-RETRY-14..16). Until now the document could state a block per provider and every route still ran the general one, so the keys TC-RETRY-8..13 resolved changed nothing a turn did. A route whose provider wrote a block never reads the general block at all, in either direction: the block may allow a retry the general one refuses, and refuse one it allows.
