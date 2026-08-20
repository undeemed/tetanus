# Turn flow - software design description

## 1. Identification

- **System:** tetanus, Phase ① core.
- **Component:** the turn engine (`crates/turn`), its extension points, and the durable session events one turn writes.
- **Version:** 0.1.0, tracking upstream deepseek-harness `0.1.0-rc.7`.
- **Status:** implemented and covered by the conformance suite (`crates/turn/tests/turn_flow.rs`).
- **Authoritative copy:** this file, in the tetanus repository.
  Upstream `docs/architecture.md` and `docs/agent-lifecycle.md` stay authoritative for *parity*; where they disagree, section 6 records the decision.

## 2. Stakeholders and their design concerns

| Stakeholder | Concern | Answered by |
| --- | --- | --- |
| Plugin author | Which events fire, in what order, and which ones can change the outcome | §4.1, §4.2 |
| Conformance reviewer | What exact sequence a compliant driver emits | §4.1, §5 |
| Harness maintainer | Which components the driver resolves, and where the seams are | §4.3 |
| Session/UI consumer | Which events are durable, and how model history is rebuilt | §4.4 |
| Parity reviewer | How this matches upstream, and where it deliberately does not | §6 |

## 3. Definitions

A **step** is one model request plus the tools it calls.
A **turn** is zero or more steps.
It opens before its first input is claimed and closes when nothing is owed.

## 4. Design views

### 4.1 Interaction view - the canonical sequence

```text
turn/start
  claim next-step input plus one queued message
  -> agent/pre-step                   reject | enter(messages)
     reject, or a first enter rewritten empty -> close the turn with no step
     step/start
     append entered messages as user/message
     derive model history from the log
     assemble prompt sections + tool schemas   system-prompt/assemble
     agent/request -> llm/stream -> assistant/chunk* -> assistant/message
     tool/call* -> tools/pre-execute -> tools/execute -> tools/post-execute -> tool/result*
     step/end
     tools owe another request, or next-step input arrived -> claim -> next step
  -> agent/turn-stopping
turn/end
```

The mock adapter turns that shape into one exact sequence, held in `crates/turn/tests/harness/mod.rs` as `MOCK_TURN_FLOW` and asserted whole by TC-TURN-1.
Step 1 calls a tool, so the tool pipeline and the loop-back both run; step 2 answers and the turn closes.

`agent/turn-stopping` fires only when the turn spent at least one step.
A turn closed by a rejected first claim emits `turn/start`, `agent/pre-step`, `turn/end` and nothing else (TC-TURN-4).

A model request that no recovery listener saved ends the turn where it failed.
The closers still run: the step the failure interrupted gets its `step/end`, and then `turn/end` carries `stop_reason: "failed"` (TC-CLOSE-1, TC-CLOSE-5).
`agent/turn-stopping` is not offered such a turn, because the checkpoint is where a listener may hold a turn open and this one is already over (TC-CLOSE-2).

### 4.2 Dependency view - the four dispatch modes

Upstream documents four modes for plugin authors.
Each event below is declared with exactly one of them, and the bus panics when an event is dispatched through another (`assert_mode`, TC-BUS-MODE-1).

| Event | Mode | Output | Purpose |
| --- | --- | --- | --- |
| `session/event` | emit | none | every durable append, in registration order |
| `session/flush` | parallel | none | the durability barrier; all listeners awaited together |
| `agent/pre-step` | waterfall | `PreStepDecision` | rewrite or reject the claimed messages |
| `system-prompt/assemble` | waterfall | `SystemPrompt` | add prompt sections and tool schemas |
| `agent/request` | waterfall | `ModelRequest` | change the request before it leaves |
| `llm/stream` | waterfall | `Result<ModelResponse, LlmError>` | wrap, replace or record the provider call |
| `tools/pre-execute` | waterfall | `ToolCall` | rewrite a call |
| `tools/execute` | waterfall | `Result<ToolOutcome, ToolError>` | replace the executor |
| `tools/post-execute` | waterfall | `ToolOutcome` | rewrite a result |
| `agent/turn-stopping` | serial | `Option<TurnStopVeto>` | veto the close; the first bail wins |

Waterfall listeners must call `next()` to delegate.
A listener that returns without calling it has vetoed the built-in behaviour, which is how `llm/stream` can replace the provider entirely (TC-TURN-5).

`session/flush` is not part of a turn.
It hangs off `TurnEngine::flush()`, so the asserted trace stays exactly the documented turn flow.

### 4.3 Composition view - what the driver resolves

The driver names no concrete adapter, tool set or storage backend.
`boot()` mounts four plugins in dependency order onto one context; `TurnEngine::from_context` then resolves three typed services.

| Service | Key | Provider | Phase ① implementations |
| --- | --- | --- | --- |
| `LlmService` | `llm` | `dyn LlmAdapter` | `MockAdapter`, `DeepSeekAdapter` |
| `ToolsService` | `tools` | `ToolRegistry` | `EchoTool` |
| `SessionService` | `sessions` | `dyn SessionLog` | `JsonlSessionLog` |

`AgentLoopPlugin` declares the other three as dependencies and re-checks them at start, so a missing provider fails at boot naming `agent-loop`, not at the first turn (TC-BOOT-3).
Swapping the adapter is therefore a boot-time change with no edit to the engine (TC-BOOT-5).

### 4.4 Information view - the durable log

Durable events are `turn/*`, `step/*`, `user/message`, `assistant/*` and `tool/*`.
Each append writes one JSONL line and fsyncs it, then dispatches `session/event`.

```json
{"type":"assistant/message","seq":7,"time":1755558000123,"data":{...},"sourceEventSeqs":[3,4,5,6]}
```

- `seq` equals the index of the line, so a replay verifies contiguity (TC-TURN-2).
- Raw `assistant/chunk` events are kept, so a UI can replay the stream exactly as it arrived.
- `assistant/message` cites the chunks it was assembled from, and `tool/result` cites its `tool/call` (TC-TURN-3).
- Model history is *derived* from the log by `derive_messages`, never stored twice.
  Model-visible means logged.

## 5. Verification

| Concern | Test case |
| --- | --- |
| Full documented sequence | TC-TURN-1 |
| Durability and replay | TC-TURN-2 |
| Event provenance | TC-TURN-3 |
| Turn with no step | TC-TURN-4 |
| Waterfall veto at `llm/stream` | TC-TURN-5 |
| Serial veto at `agent/turn-stopping` | TC-TURN-6 |
| Many turns in one journal | TC-TURN-7 |
| The four modes | TC-BUS-EMIT-1/2, TC-BUS-PARALLEL-1, TC-BUS-SERIAL-1/2, TC-BUS-WATERFALL-1/2/3, TC-BUS-MODE-1 |
| Registry composition | TC-BOOT-1 to TC-BOOT-5 |
| Provider protocol | TC-DS-WIRE-1/2, TC-DS-SSE-1, TC-DS-DECODE-1/2, TC-DS-ADAPTER-1, TC-DS-CRED-1, TC-DS-LIVE-1 |
| Headless run | TC-CLI-1 to TC-CLI-3 |

## 6. Design rationale

### 6.1 Where `system-prompt/assemble` sits

Upstream disagrees with itself.
`docs/architecture.md` puts "assemble prompt sections + tool schemas" *before* `agent/pre-step`, as an unnamed action in the claim block.
`docs/agent-lifecycle.md` names the event `system-prompt/assemble` and puts it *inside* the step, after `user/message` and before `agent/request`.

tetanus follows agent-lifecycle.md, for three reasons.
It is the only place upstream names the event at all, so it is the only statement about the event as such.
Architecture.md itself says "Each step reads the prompt sections and tool schemas that plugins registered", which is per-step, not per-claim.
And assembling before `agent/pre-step` would build a prompt for a claim that `agent/pre-step` may reject, wasting the work and letting listeners observe a prompt for a step that never happens.

### 6.2 Four modes, not five

Upstream's low-level `docs/cordis-api/events.md` lists a fifth mode, `bail`.
The harness primer (`docs/cordis-primer.md`) documents four to plugin authors: emit, waterfall, parallel, serial.
Phase ① implements the four a plugin author can use.
`bail` semantics are reachable through serial, whose first bail wins.

### 6.3 Streaming without a channel

The adapter writes chunks into a `&mut dyn ChunkSink` carried inside the `llm/stream` payload.
The alternative, an mpsc channel per request, was rejected: it adds a task and a buffer, makes chunk order depend on scheduling, and would put a lock across an await point in the logging sink.
With the sink in the payload the driver's `LogSink` is reached through `&mut ev`, so chunk order is deterministic and a `llm/stream` listener can substitute the whole call without touching the transport.

### 6.4 The tracer is shared, not duplicated

`TurnTrace` (`crates/turn/src/trace.rs`) attaches one delegating listener per documented event.
Both the conformance suite and `tetanus run` use it, so the sequence a reviewer reads on the terminal is produced by the same code the merge gate asserts.
The alternative, a private observer in the test plus separate printing in the CLI, was rejected: the two would drift.

### 6.5 The mock adapter calls a tool

A one-step mock would never exercise `tools/*` or the loop-back.
The mock therefore asks for `echo` on step 1 and answers on step 2, so the offline run and the merge gate both cover the complete documented sequence with no key and no network.

## 7. Out of scope for Phase ①

Left as seams, not implemented: layered config recompose at run time, reversible effects beyond registration handles, the full tool pipeline (permissions, concurrency, cancellation), further adapters, the JSON-RPC and WebSocket surfaces, and the WASM host.
