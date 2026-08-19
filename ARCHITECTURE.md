# Architecture

## 1. Identification

- **System:** tetanus, the whole Cargo workspace.
- **Version:** 0.1.0, Phase ①, tracking upstream deepseek-harness `0.1.0-rc.7`.
- **Status:** implemented and covered by 46 offline tests.
- **Authoritative copy:** this file, in the tetanus repository.
- **Scope:** the shape of the system. The turn itself has its own design description in
  [docs/turn-flow.md](docs/turn-flow.md); this file does not repeat it.

## 2. Stakeholders and their design concerns

| Stakeholder | Concern | Answered by |
| --- | --- | --- |
| New contributor | Which crate owns what, and where to start reading | §4.2 |
| Plugin author | How a component joins the system and what it may hook | §4.3, §4.5 |
| Provider integrator | What a model adapter must implement | §4.6 |
| UI or tooling author | What surfaces exist today, and what a consumer can observe | §4.7, §4.8 |
| Parity reviewer | How this maps to upstream, and where it deliberately differs | §4.1, §6 |
| Release reviewer | What the merge gate actually proves | §5 |

## 3. Definitions

A **step** is one model request plus the tools it calls.
A **turn** is zero or more steps.
A **plugin** is a unit of composition mounted at boot.
A **service** is a swappable capability resolved by type.
An **effect** is a registration that unwinds when its handle drops.

## 4. Design views

### 4.1 Context view - tetanus and upstream

Upstream deepseek-harness is a TypeScript monorepo on the [Cordis](https://github.com/cordiverse/cordis)
plugin runtime.
tetanus takes upstream's published documents as the specification and owns everything below them.
It shares no code, no wire protocol, and no plugin ecosystem with upstream.

What is carried across: the event taxonomy (durable session events versus live extension points), the
turn flow, the four dispatch modes, the append-only session log, the guarded tool pipeline, and
reversible effects.

What is deliberately dropped: the generated `/api` wire contract, the Node plugin ecosystem, and
TypeScript-style hot module reload of core code.
The reasoning and the rejected alternatives are in [docs/PLAN.md](docs/PLAN.md).

Where the two upstream documents disagree, tetanus records the pick rather than picking silently.
The one live case is the position of `system-prompt/assemble`
([docs/turn-flow.md](docs/turn-flow.md) section 6.1).

### 4.2 Composition view - the workspace

```text
crates/cli      tetanus-hardness   the `tetanus` binary
  -> crates/turn     tetanus-turn      turn engine, events, LLM seam, tools, boot, trace
       -> crates/session  tetanus-session   durable event vocabulary + JSONL journal
       -> crates/core     tetanus-core      registry, services, event bus, effects
  -> crates/config   tetanus-config    layered config with provenance
  -> crates/engine   tetanus-engine    the `Engine` implementation behind the contract
  -> crates/rpc      tetanus-rpc       JSON-RPC codec and carriers, hosted by `tetanus serve`
  -> crates/ui       tetanus-ui        colour policy, theme, width, redrawable block, scrollable page,
                                       full-screen view loop

crates/protocol   tetanus-protocol   the engine/presentation contract (§4.8)
```

`tetanus-core` depends on nothing in the workspace.
`tetanus-config` depends on no other workspace crate; the CLI and the engine both read it.
It holds one document per layer rather than one folded map, because a layer that is re-read can
*drop* a key and the value under it has to come back; a folded map has nothing to come back to
([crates/config/src/lib.rs](crates/config/src/lib.rs)).
`tetanus-protocol` deliberately depends on no engine crate, so refactoring the engine cannot break a
surface.
`tetanus-ui` holds the same line from the other side: it depends on no engine crate and holds no
engine type, so it formats what it is given and the two lanes stay independently reviewable.
Nothing depends on `tetanus-hardness`.

### 4.3 Logical view - composition primitives

Four primitives in `tetanus-core`, each one small enough to read in a sitting.

| Primitive | Source | What it guarantees |
| --- | --- | --- |
| `Registry` / `Plugin` | [crates/core/src/registry.rs](crates/core/src/registry.rs) | Plugins mount in topological dependency order. Cycles, duplicates, and missing dependencies are rejected at boot, naming the plugin. A plugin that fails to start rolls the pass back, unmounting dependents before dependencies. |
| `Services` / `Service` | [crates/core/src/services.rs](crates/core/src/services.rs) | A capability is keyed by type, with a human-readable `KEY`. Exactly one provider per definition; a second is a wiring error. Consumers resolve by type and never import an implementation. |
| `EventBus` / `Event` | [crates/core/src/events.rs](crates/core/src/events.rs) | An event declares its dispatch mode as a `const`. Registering or dispatching through another mode panics rather than silently doing nothing. |
| `EffectHandle` / `EffectScope` | [crates/core/src/effects.rs](crates/core/src/effects.rs) | Every registration returns a handle; dropping it unwinds the registration. A scope holds several and unwinds them newest first, nests inside another scope as one handle, and finishes the unwind even if an undo panics. `Context` is a scope's owner, so a plugin's wiring dies with the context. |

`Context` ([crates/core/src/context.rs](crates/core/src/context.rs)) is what a boot pass hands each
plugin: the service registry, the bus, and the owned effect handles.

The four dispatch modes are `emit`, `parallel`, `serial`, and `waterfall`.
Waterfall listeners are around-middleware over a built-in terminal: call `next()` to delegate, or
return your own value to veto the rest of the chain and the terminal with it.
That is how a listener can replace a provider call entirely.
Upstream's low-level API doc lists a fifth mode, `bail`; the primer documents four to plugin authors,
and `bail` semantics are reachable through `serial`.

### 4.4 Interaction view - the turn

The turn engine ([crates/turn/src/engine.rs](crates/turn/src/engine.rs)) is the only component that
owns event order:

```text
turn/start -> agent/pre-step -> step/start -> user/message -> system-prompt/assemble
  -> agent/request -> llm/stream -> assistant/chunk* -> assistant/message
  -> tool/call* -> tools/pre-execute -> tools/execute -> tools/post-execute -> tool/result*
  -> step/end -> ...loop... -> agent/turn-stopping -> turn/end
```

The canonical sequence, each event's dispatch mode and output type, and the rationale for the order
are in [docs/turn-flow.md](docs/turn-flow.md).
The live extension points are declared in [crates/turn/src/events.rs](crates/turn/src/events.rs).

The engine resolves three services from the registry and names no implementation:

| Service | Key | Provider trait | Phase ① implementations |
| --- | --- | --- | --- |
| `LlmService` | `llm` | `dyn LlmAdapter` | `MockAdapter`, `DeepSeekAdapter` |
| `ToolsService` | `tools` | `ToolRegistry` | `EchoTool` |
| `SessionService` | `sessions` | `dyn SessionLog` | `JsonlSessionLog` |

`boot()` ([crates/turn/src/boot.rs](crates/turn/src/boot.rs)) mounts the three providers plus
`AgentLoopPlugin`, which provides nothing and declares the other three as dependencies, so a missing
provider fails at boot naming `agent-loop` rather than mid-turn.

### 4.5 Information view - the session log

`SessionEvent` ([crates/session/src/lib.rs](crates/session/src/lib.rs)) is the durable record: a
`type`, a `seq` equal to the log length at append time, a `time` in epoch milliseconds, a JSON `data`
payload, and `sourceEventSeqs` on the surface events that cite their inputs.

`JsonlSessionLog` writes one JSON line per event, fsyncs it, mirrors it in memory, then emits
`session/event` on the bus, so observers never poll the file.
`replay()` reads a journal back and verifies `seq` contiguity: a gap means the file is not a faithful
copy of the log that produced it.

Model history is *derived* from the log by `derive_messages`
([crates/turn/src/log.rs](crates/turn/src/log.rs)), never stored beside it.
Model-visible means logged.
Raw `assistant/chunk` events stay on the log so a UI can replay a stream exactly as it arrived, while
the `assistant/message` that cites them is what enters history.

### 4.6 Interface view - the LLM adapter seam

A provider implements `LlmAdapter` ([crates/turn/src/llm/mod.rs](crates/turn/src/llm/mod.rs)): a
provider route, an advisory model catalog, and one `stream()` call that writes `StreamChunk`s into a
`&mut dyn ChunkSink` and returns a `ModelResponse`.
Adding a provider means implementing that trait and providing it as the `llm` service at boot.

Chunks travel through a sink carried inside the `llm/stream` payload rather than a channel, so chunk
order is deterministic and does not depend on task scheduling
([docs/turn-flow.md](docs/turn-flow.md) section 6.3).
The engine's sink turns every chunk into a durable `assistant/chunk`.

Two adapters ship:

- [crates/turn/src/llm/mock.rs](crates/turn/src/llm/mock.rs) - deterministic and offline. It calls a
  tool on step 1 and answers on step 2, so one offline run covers the tool pipeline and the loop-back.
- [crates/turn/src/llm/deepseek.rs](crates/turn/src/llm/deepseek.rs) - DeepSeek chat completions
  behind an `SseTransport` seam, so the request body and the stream decoder are tested without
  network. Credentials are referenced by environment variable name; config never carries a literal
  key.

Every failure carries a stable code (`LlmError::code`), and
[crates/turn/src/llm/retry.rs](crates/turn/src/llm/retry.rs) decides from that code whether another
attempt is worth making and how long to wait first.
The policy is a value that decides, not a loop that waits: it returns the delay instead of sleeping,
which is what keeps its cases offline and free of a clock.
The executor that would act on the decision is phase ② ([docs/parity.md](docs/parity.md) section 4).

### 4.7 Interface view - surfaces

The only surface today is the `tetanus` binary ([crates/cli/src/main.rs](crates/cli/src/main.rs)).
It carries the eight subcommands [docs/interface-contract.md](docs/interface-contract.md) §4.7
defines - `run`, `sessions`, `replay`, `models`, `tools`, `config`, `serve`, `info` - each identified
there by the contract calls it makes rather than by what it prints. See [README.md](README.md#cli).

`tetanus run` shows one turn three ways, and every settled line in all three comes from the same
`Reader` in [crates/cli/src/render/timeline.rs](crates/cli/src/render/timeline.rs), so a turn watched
live reads like the same turn replayed tomorrow.
The default is a block under the shell prompt, redrawn in place by `Screen`.
`--ui` takes the whole terminal instead and composes each frame with `Page`
([crates/ui/src/page.rs](crates/ui/src/page.rs)), which is what makes a turn scrollable while it is
still running.
`--json` prints the contract's own result types and draws nothing.
All three read their events from the session log the engine is writing rather than from the bus: the
journal is the durable record, and polling it is what keeps the presentation lane out of the engine.

`tetanus run` also observes the sequence with `TurnTrace`
([crates/turn/src/trace.rs](crates/turn/src/trace.rs)), one delegating listener per documented event,
which `--trace` prints instead of the turn.
Any other consumer would attach the same way: `session/event` for durable facts, the waterfalls for
live participation.

`tetanus-engine` ([crates/engine](crates/engine)) implements the `Engine` trait, and `tetanus-rpc`
([crates/rpc](crates/rpc)) carries it: a JSON-RPC 2.0 codec with a stdio carrier and a WebSocket
carrier. `tetanus serve` hosts the stdio one, and `tetanus serve --listen` the WebSocket one. There
is no web UI. §4.8 covers the contract all three speak.

### 4.8 Interface view - the engine/presentation contract

`tetanus-protocol` ([crates/protocol](crates/protocol)) is the machine-readable half of
[docs/interface-contract.md](docs/interface-contract.md): the JSON-RPC 2.0 envelope, the wire types, a
capability list, and an `Engine` trait that every carrier drives.
One contract serves three carriers - in process, stdio, WebSocket - because a subscription takes an
`EventSink` supplied by the carrier rather than named on the wire.

The document is the specification and the crate is what both lanes compile against.
The crate carries the types and the trait; `tetanus-engine` implements every call, and `tetanus-rpc`
serves them over stdio and WebSocket.
The contract's own status table and changelog are authoritative for what is served.

A boundary change is its own pull request touching the document and the types together
([AGENTS.md](AGENTS.md)).

## 5. Verification - the conformance approach

Parity with upstream is asserted, not asserted about.

- **The sequence is a constant.** `MOCK_TURN_FLOW` in
  [crates/turn/tests/harness/mod.rs](crates/turn/tests/harness/mod.rs) is the entire expected event
  sequence of one turn, compared by equality. Moving an event fails the build until the constant is
  changed on purpose.
- **One tracer, two readers.** The suite and `tetanus run` read the same `TurnTrace`, so the printed
  sequence and the asserted sequence cannot drift.
- **The mock is a full turn, not a stub.** It exercises the tool pipeline and the loop-back, so the
  gate covers the complete documented flow with no key and no network.
- **Offline is a hard rule.** All 32 cases run with no credentials. The single live provider case
  reports itself skipped without `DEEPSEEK_API_KEY`, so CI stays a gate.
- **Cases are identified.** Every case carries a stable `TC-*` identifier and an explicit expected
  result; [docs/turn-flow.md](docs/turn-flow.md) section 5 and
  [docs/interface-contract.md](docs/interface-contract.md) section 6 map concerns to case identifiers.

Coverage today: the turn sequence and its edge cases, the four dispatch modes and mode enforcement,
registry composition and boot failure, the DeepSeek wire format and SSE decoding, the contract's wire
shapes and version compatibility, and the headless run.
[CONTRIBUTING.md](CONTRIBUTING.md) lists the commands.

## 6. Design rationale

Three decisions shape everything above; alternatives that were rejected are recorded with them.

**Compile-time composition instead of runtime duck-typing.** Upstream resolves services by string key
on a shared context, so a wiring mistake surfaces on the first turn that needs it. tetanus keys a
service by type and rejects duplicates and missing providers at boot. The cost is that out-of-tree
plugins need a host; a WASM component host is the Phase ③ answer, and the price was accepted
explicitly in [docs/PLAN.md](docs/PLAN.md).

**The dispatch mode is part of the contract.** It could have been a runtime string, matching
upstream. Making it a `const` on the `Event` impl means the wrong mode is a panic at registration,
not a listener that silently never runs.

**Our own protocol, not upstream's.** Upstream's web contract is generated from TypeScript
decorators, so it is a build artifact rather than a documented protocol and can change on any rc bump.
Adopting it would have bought a finished UI at the cost of pinning to one upstream commit. tetanus
owns its surfaces instead. This is the single largest divergence and the reason parity is functional,
not protocol-level.

## 7. Not built yet

Layered config recompose at run time, live subtree remount, the full tool pipeline (permissions,
concurrency, cancellation), further adapters, MCP, sandboxing, the web UI, and the WASM plugin host.
[README.md](README.md#current-status) has the status table; [docs/PLAN.md](docs/PLAN.md) has the phase
plan.
