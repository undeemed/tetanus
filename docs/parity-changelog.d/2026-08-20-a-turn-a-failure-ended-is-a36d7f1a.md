---
date: 2026-08-20
order: 32
---
A turn a failure ended is closed on the journal (`TurnEngine::close`, TC-CLOSE-1..5): the step the failure interrupted gets its `step/end`, then `turn/end` reads `stop_reason: "failed"`. Until now a failed turn returned its error and left `turn/start` open, so the state machine `docs/interface-contract.md` §4.6 documents held only for a turn that succeeded, and the next open synthesized closers reading `interrupted` for a turn nothing interrupted. Upstream closes the same two in a `finally` (`packages/core/agent-loop/src/agent.ts`). The reason is a value on the journal, not a `StopReason` variant: a failed turn produces no `TurnOutcome` to carry one.
