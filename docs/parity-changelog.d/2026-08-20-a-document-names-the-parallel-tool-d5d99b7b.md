---
date: 2026-08-20
order: 46
---
A document names the parallel tool cap (`agent.max_parallel_tool_calls`, TC-PARALLEL-1..5, opening `core/agent-loop/tests/settings.spec.ts` in section 4). It is the tool-order gap again: the cap reached `TurnConfig` only from a composer in Rust, so no deployment could ask for serial dispatch. Zero is refused when the settings are resolved rather than honoured as a pool that can start nothing, and `config.dump` publishes the key even when nothing sets it. `Catalogs::new` now takes the resolved `EngineConfig` instead of a list of values, so a key the engine settles cannot be added and then forgotten by the dump.
