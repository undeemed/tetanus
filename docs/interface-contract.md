# Interface contract: engine and presentation

## 1. Identification

- **System:** tetanus 0.1.0.
- **Component:** the boundary between the engine lane (harness runtime, agent loop, tool registry, session log, RPC server) and the presentation lane (fire UI, CLI rendering).
- **Machine-readable half:** the `tetanus-protocol` crate (`crates/protocol`).
This document is the specification; the crate is what both lanes compile against.
They must agree, and section 8 records every change to either.
- **Status:** contract version 1.0.
Section 4.2 marks, per call, whether the engine serves it yet.
- **Authoritative copy:** this file, in the tetanus repository.

## 2. Stakeholders and their concerns

| Stakeholder | Concern | Answered by |
| --- | --- | --- |
| Presentation lane | Which calls exist, what they return, and what must be rendered | §4.2, §4.3, §4.4 |
| Presentation lane | What may change under it without warning, and what may not | §5 |
| Engine lane | What it has promised to serve, and in which shape | §4.2, §4.5 |
| Either lane, mid-change | Where a boundary change lands, and who reviews it | §3, §8 |
| Reviewer | Whether a call is served or only reserved | §4.2 |
| Conformance reviewer | Which clause each test case fixes | §6 |

## 3. The lane boundary

The engine lane publishes this contract.
The presentation lane consumes it and reviews it.
Neither lane edits the other's code.

Three rules follow.

1. **A boundary change is its own pull request.**
It touches this document and the `tetanus-protocol` types together, adds a row to section 8, and lands before any feature that depends on it.
A boundary change never arrives buried inside a feature pull request.
2. **The contract carries facts, never rendering.**
Colour, layout, spinner style, column widths, help wording and progress bars are the presentation lane's, and no field here describes them.
If a presentation need cannot be met from the facts here, the contract is incomplete: extend it, do not work around it.
3. **`tetanus-protocol` depends on no engine crate.**
It has three dependencies: `serde`, `serde_json`, `async-trait`.
The engine converts its internal types into these wire shapes, so refactoring the engine is not a breaking change for a surface.

## 4. Design views

### 4.1 Context view: carriers

One contract, three carriers.
Every carrier moves the same payloads, so a surface that works over one works over the others.

| Carrier | Who uses it | Framing |
| --- | --- | --- |
| In process | the `tetanus` binary | direct calls on the `Engine` trait, no serialization |
| stdio | an editor or a script driving the binary | JSON-RPC 2.0, one object per line, UTF-8, no embedded newlines |
| WebSocket | the fire UI | JSON-RPC 2.0, one object per text frame |

The envelope is JSON-RPC 2.0 exactly: `jsonrpc`, `id`, `method`, `params`, `result`, `error`.
A frame whose `jsonrpc` is absent or is not the string `"2.0"` is rejected, not guessed at.
Batch arrays are not part of contract 1.0.

Both peers demultiplex incoming frames with `rpc::Message`, because the server may also call the client (§4.4.3).

### 4.2 Interface view: the calls

`Served` means this build answers the call.
`Reserved` means the shape is frozen and a surface may build against it, but the engine answers `NotImplemented` (`-32001`) until the slice that serves it lands.
A surface checks a capability string from `rpc.hello` before it uses a reserved call, and hides the affordance when it is absent.

#### Client to server

| Method | Params | Result | Capability | Status |
| --- | --- | --- | --- | --- |
| `rpc.hello` | `HelloParams` | `HelloResult` | always | Served |
| `session.create` | `SessionCreateParams` | `SessionInfo` | always | Served |
| `session.list` | none | `SessionListResult` | always | Served |
| `session.events` | `SessionEventsParams` | `SessionEventsResult` | always | Served |
| `session.subscribe` | `SessionSubscribeParams` | `SessionSubscribeResult` | `session.subscribe` | Served on a carrier that can push |
| `session.unsubscribe` | `SessionRef` | `Ack` | `session.subscribe` | Served on a carrier that can push |
| `agent.prompt` | `AgentPromptParams` | `AgentPromptResult` | always | Served |
| `agent.status` | `SessionRef` | `AgentStatusResult` | always | Served |
| `agent.interrupt` | `SessionRef` | `Ack` | `agent.interrupt` | Served |
| `catalog.tools` | none | `ToolCatalogResult` | always | Served |
| `catalog.models` | none | `ModelCatalogResult` | always | Served |
| `config.dump` | none | `ConfigDumpResult` | always | Served |

A call with no params accepts an absent `params`, or `{}`, and treats them alike.

`session.subscribe` and `session.unsubscribe` are the only calls absent from the `Engine` trait.
They bind a stream to one connection, which an in-process caller does not have; that caller listens on the event bus directly.

#### Server to client

| Frame | Kind | Params | Reply | Status |
| --- | --- | --- | --- | --- |
| `session/event` | notification | `SessionEventPush` | none | Served |
| `agent/status` | notification | `AgentStatusPush` | none | Served |
| `ui/ask` | request | `AskParams` | `AskResult` | Reserved, capability `ui.ask` |

A client that receives an unknown notification method ignores it.
A client that receives an unknown *request* method answers `MethodNotFound` (`-32601`).
The distinction matters: the engine may add a notification in a minor version, and an old surface must survive it.

### 4.3 Information view: shared types

Every type below lives in `tetanus-protocol` with the field names its JSON uses.
The crate is authoritative for field-level detail; this section states the invariants a reader cannot see in a struct.

**`SessionEvent`** is one durable fact, byte-identical to one line of the JSONL journal.
`type` stays a free string because the durable vocabulary grows, and a surface must pass an unknown type through rather than drop it.
`seq` equals the index of the line, so a replay verifies contiguity.
`sourceEventSeqs` keeps its camel case, and is present only on surface events (`user/message`, `assistant/message`, `tool/result`); an `assistant/message` may cite a known-empty list.

The durable vocabulary a surface renders today: `session/start`, `turn/start`, `step/start`, `user/message`, `assistant/chunk`, `assistant/message`, `tool/call`, `tool/result`, `step/end`, `turn/end`.
`session/start` is the first line of every journal and carries the session header, so listing a cold session reads the log and never a sidecar file.
`assistant/chunk` is the streaming surface.
Raw chunks stay on the log, so a surface replays a stream exactly as it arrived rather than re-deriving it.
There is no separate progress event: progress is `step/start`, the chunks, `tool/call`, `tool/result` and `step/end`, in order.

**`AgentState`**, **`StopReason`** and **`ConfigLayer`** each carry an `Other(String)` fallback.
A surface renders the fallback rather than failing, and that is exactly what lets the engine add a variant in a minor version.

**`SessionInfo`** answers a list view without reading a journal: id, journal path, provider, model, creation time, `last_seq` (`-1` for an empty log), and live state.

**`TurnSummary`** is the closing shape of one turn.
Every field is also reconstructable from the journal; the summary is the convenience form.

**`ToolDescriptor`**, **`ProviderDescriptor`** and **`ConfigEntry`** are what a help surface, a model picker and `tetanus config` render.
`ProviderDescriptor.available` is false when a provider is registered but its credential is absent, so a picker can grey the entry instead of failing at the first turn.

**`Question`**, **`QuestionOption`** and **`Answer`** are the ask vocabulary of §4.4.3.
`label` is both the user-facing text and the value the answer carries, so a caller reads the same field whatever the surface renders.

### 4.4 Interaction view

#### 4.4.1 Opening

```text
client -> rpc.hello { protocol_version, client }
server -> HelloResult { protocol_version, server, capabilities }
```

`rpc.hello` is the first call on a connection.
A server that receives any other call first answers `InvalidRequest` (`-32600`).
A server whose major version differs from the client's answers `UnsupportedProtocolVersion` (`-32000`) and closes.

#### 4.4.2 One turn

```text
client -> session.create { model }              -> SessionInfo
client -> session.subscribe { session_id }      -> { last_seq }
client -> agent.prompt { session_id, content }

  server -> agent/status  running
  server -> session/event turn/start
  server -> session/event step/start
  server -> session/event user/message
  server -> session/event assistant/chunk   (one per delta)
  server -> session/event assistant/message
  server -> session/event tool/call
  server -> session/event tool/result
  server -> session/event step/end
  server -> session/event turn/end
  server -> agent/status  idle

server -> AgentPromptResult { summary }
```

`agent.prompt` returns when the turn closes.
Its events arrive meanwhile, so a surface renders progress without polling.
A surface that only wants the answer can ignore the pushes and read the result.

The pushed order inside a turn is the engine's documented turn flow, which `docs/turn-flow.md` specifies and the conformance suite asserts by equality.
This contract does not restate it, so the two cannot drift.

A second `agent.prompt` while a turn is in flight is answered `SessionBusy` (`-32003`).
Queueing a follow-up into the running turn's inbox is a later addition, and will arrive as a new call rather than a change to this one.

`agent.interrupt` stops the turn at the next step boundary.
It does not abort an in-flight provider call in contract 1.0.
The turn still closes normally: `turn/end` carries `stop_reason: "cancelled"`, and `agent.prompt` returns its summary rather than an error.
`Cancelled` (`-32004`) is therefore not raised by an interrupted prompt; it is reserved for a call that could not complete at all.

#### 4.4.3 Asking the user

```text
server -> ui/ask { session_id, questions }   (a request, not a notification)
client -> AskResult { answers }
```

The ask is a server-to-client request because the engine blocks on it: a tool cannot proceed until the human decides.
A client that advertises no `ui.ask` capability is never asked, and the engine denies the underlying action instead of hanging.
A client that advertises the capability and then fails to answer must answer with an error; the engine treats any error as a denial.

### 4.5 Error view

Every failure is a JSON-RPC error object: `code`, `message`, `data`.
`message` is a plain sentence for a log.
It is not a rendering: the presentation lane may replace it with its own wording, keyed on the code.

| Code | Name | `data` | Exit status |
| --- | --- | --- | --- |
| -32700 | `ParseError` | none | 2 |
| -32600 | `InvalidRequest` | none | 2 |
| -32601 | `MethodNotFound` | `{ method }` | 2 |
| -32602 | `InvalidParams` | `{ field }` when one field is at fault | 2 |
| -32603 | `Internal` | none | 1 |
| -32000 | `UnsupportedProtocolVersion` | `{ server, client }` | 3 |
| -32001 | `NotImplemented` | `{ method }` | 3 |
| -32002 | `SessionNotFound` | `{ session_id }` | 4 |
| -32003 | `SessionBusy` | `{ session_id, turn }` | 4 |
| -32004 | `Cancelled` | none | 130 |
| -32005 | `MissingCredential` | `{ provider, env }` | 5 |
| -32006 | `ProviderError` | `{ provider, status }` | 6 |
| -32007 | `ToolUnknown` | `{ name }` | 4 |
| -32008 | `LogCorrupt` | `{ session_id, line }` | 1 |
| -32009 | `Io` | `{ path }` when a path is at fault | 1 |

A code's meaning is frozen for the life of a major version.
A surface that meets a code it does not know reports the raw code and message, and exits 1; `RpcError::kind()` returns `None` rather than remapping it onto a known code.

The exit-status column is the contract, not a suggestion.
`ErrorCode::exit_status()` is the single source, so no surface invents its own.

A failed tool call is not an error.
It is a `tool/result` with `ok: false`, because it is a binding rejection the model sees, not a failure of the call the surface made.

### 4.6 State dynamics

A session is `idle` or `running`.

```text
idle  --agent.prompt-->  running  --turn/end-->  idle
```

`agent/status` is pushed on every transition, and `agent.status` reads the same value.
The state is live, not durable: it is not derivable from the journal while a turn is in flight, which is why it is pushed rather than folded from events.
A surface that missed a push resynchronises with `agent.status`.

### 4.7 The CLI boundary

The `tetanus` binary is an in-process client of the same `Engine` trait the RPC server wraps.
Each subcommand is defined as the calls it makes, and makes no others.

| Subcommand | Calls |
| --- | --- |
| `tetanus run` | `session.create`, `agent.prompt`, and the event bus for live rendering |
| `tetanus replay <path>` | `session.events` |
| `tetanus sessions` | `session.list` |
| `tetanus tools` | `catalog.tools` |
| `tetanus models` | `catalog.models` |
| `tetanus config` | `config.dump` |
| `tetanus serve` | hosts the stdio and WebSocket carriers |
| `tetanus info` | none; build metadata only |

Machine-readable output is contract output.
`--json` prints the call's result type verbatim, one JSON object per line, with no added fields and no colour.
Human-readable output is the presentation lane's, and this document says nothing about it beyond the exit statuses in §4.5.

#### File ownership

Two lanes share one binary, so ownership is by file and not by judgement.

| Path | Owner | Holds |
| --- | --- | --- |
| `crates/core`, `crates/config`, `crates/session`, `crates/turn` | engine | the runtime |
| `crates/protocol` | engine | this contract, as types |
| `crates/engine` | engine | the `Engine` implementation |
| `crates/rpc` | engine | the JSON-RPC codec and the stdio and WebSocket carriers |
| `crates/cli` | presentation | the whole binary: argv, rendering, help text, and the wiring to the crates above |

The engine lane publishes libraries.
It writes no `println!` outside a test, and it does not own a binary.
The presentation lane owns the binary and wires each subcommand to the calls §4.7 lists for it.

Neither lane edits the other's files.
A change that seems to need one is a gap in this document, and the fix is a contract pull request.

## 5. Versioning and compatibility

`PROTOCOL_VERSION` is `major.minor`.
Contract 1.0 is the first.

A server accepts any client whose **major** matches, and refuses one whose major differs.
The minor is informational: it tells a peer which additions to expect, and gates nothing on its own.
Capability strings gate features, because a build may serve fewer calls than its version implies.

**A minor bump covers additions only.**
Adding a method, an optional field, an enum variant, a notification, a capability string, or an error code is a minor bump.

**A major bump covers everything else.**
Removing or renaming a method or field, changing a field's type, making an optional field required, narrowing an accepted value, or changing what an existing error code means is a major bump.

Three rules make additions safe, and a surface must follow all three.

1. Ignore unknown object fields.
2. Ignore unknown notification methods, and answer unknown request methods `MethodNotFound`.
3. Render unknown enum variants through the `Other(String)` fallback, and unknown error codes through their raw code.

The conformance cases in §6 hold these rules to their word.

## 6. Verification

The cases live in `crates/protocol/tests/wire.rs` and run offline.

| Clause | Case |
| --- | --- |
| §4.1 frame demultiplexing | TC-PROTO-1 |
| §4.1 the `"2.0"` tag is checked | TC-PROTO-2 |
| §4.5 error object shape and code round trip | TC-PROTO-3 |
| §5 an unknown code is not remapped | TC-PROTO-4 |
| §4.3 `SessionEvent` matches a journal line | TC-PROTO-5 |
| §5 unknown enum variants survive | TC-PROTO-6 |
| §4.2 pushes name their session | TC-PROTO-7 |
| §5 compatibility is decided by major alone | TC-PROTO-8 |
| §4.5 exit statuses | TC-PROTO-9 |

## 7. Design rationale

### 7.1 JSON-RPC 2.0, not a generated contract

Upstream's web API is a build artifact of TypeScript decorators (`docs/api-gateway.md`): Typert generates host and client contracts, and identity crosses the wire through a lookup map.
That surface is powerful and undocumented as a protocol, and every upstream release candidate can change it silently.
`docs/PLAN.md` chose option 2 for exactly this reason: our own protocol, fully owned.

JSON-RPC 2.0 was picked over a bespoke framing because it already answers request correlation, one-way notifications, bidirectional calls and a structured error object, and because a presentation surface can drive it from any language without a generator.
The cost is that it says nothing about streams; §4.4.2 answers that by streaming durable session events as notifications instead of inventing a stream type.

### 7.2 Streaming is the session log, not a second channel

The alternative was a dedicated progress event carrying rendered state.
It was rejected twice over.
It would put rendering decisions in the engine lane, which §3 forbids.
And it would be a second source of truth beside the journal, so a live view and a replayed view could disagree.
Pushing `session/event` verbatim means live and replay are the same data, and `docs/turn-flow.md` stays the one place the order is specified.

### 7.3 `agent.prompt` blocks, and events stream

The alternative was a fire-and-forget `agent.prompt` returning a turn id, with completion arriving as another push.
It was rejected because it forces every surface, including a three-line script, to implement a correlation table before it can print an answer.
Blocking costs nothing here: the carrier is asynchronous, the id correlates the reply, and a surface that wants progress subscribes.

### 7.4 An `Engine` trait, not a method-name match

The RPC server and the CLI both drive `methods::Engine`.
A hand-written match in each would let the two serve slightly different contracts, and the difference would show up as a UI bug rather than a compile error.
With the trait, adding a call is a compile error in every surface that has not handled it.

### 7.5 A fallback variant on every growable enum

Rust's `serde` rejects an unknown enum variant by default, which would make every added state a breaking change for an older surface.
`Other(String)` costs one variant and one match arm, and converts a breaking change into a minor one.
The variants that are not growable, such as the JSON-RPC envelope's `result` and `error`, deliberately have no fallback: a frame that is neither is malformed.

### 7.6 Wire types duplicated, not re-exported

`SessionEvent` exists in `tetanus-session` and again in `tetanus-protocol`.
Re-exporting would drag `tokio`, `tetanus-core` and the event bus into every consumer of the contract, and would tie the wire shape to an internal type that the engine must stay free to refactor.
TC-PROTO-5 pins the wire shape to the journal line, and the engine-side conversion is covered where it lands.

## 8. Changelog

Every boundary change adds a row here, in its own pull request.

| Version | Change |
| --- | --- |
| 1.0 | First contract: envelope, error codes and exit statuses, session and agent calls, tool, model and config catalogues, `session/event` and `agent/status` pushes, `ui/ask` reserved. |
