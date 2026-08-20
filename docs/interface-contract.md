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

Four rules follow.

1. **A boundary change is its own pull request.**
It touches this document and the `tetanus-protocol` types together, adds a row to section 8, and lands before any feature that depends on it.
A boundary change never arrives buried inside a feature pull request.
2. **The contract carries facts, never rendering.**
Colour, layout, spinner style, column widths, help wording and progress bars are the presentation lane's, and no field here describes them.
If a presentation need cannot be met from the facts here, the contract is incomplete: extend it, do not work around it.
3. **`tetanus-protocol` depends on no engine crate.**
It has three dependencies: `serde`, `serde_json`, `async-trait`.
The engine converts its internal types into these wire shapes, so refactoring the engine is not a breaking change for a surface.
4. **A surface matches the wire types, and matches them openly.**
Every enum that section 7.5 calls growable may gain a variant in a minor version, so a match on one carries a fallback arm rather than one arm per variant.
`KnownEvent` is not one of those, having no `Other` because nothing deserializes it, and it is why section 4.3.2 lands a new durable type in two steps: an open match on it is what would make those two one.
A surface never reaches past the wire type to match an engine enum instead, such as `tetanus_turn::StopReason`, for the reason section 4.5 already gives for the error case: an internal type has no fallback, so a match on one outside the engine crate stops compiling the day the engine names a new case.

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
Batch arrays are not part of contract 1.0: a server answers one `InvalidRequest`.

A frame the server cannot correlate is still answered, with `id: null`.
That covers a frame that is not JSON, a frame that is JSON but not a request, and a batch array.
`rpc::Id::Null` is that value.
Dropping such a frame silently would leave a client waiting for a reply it will never get, which is the one failure a codec must not have; a client never sends the value itself.

Pushes reach all three carriers the same way.
`session.subscribe` takes an `EventSink` alongside its params, supplied by the carrier and never by the wire.
The stdio and WebSocket carriers implement `EventSink` as "serialize and write a frame"; the in-process caller implements it as "hand to the renderer".
So one renderer serves every carrier, and no surface has to reach past `tetanus-protocol` to see a chunk arrive.

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
| `session.subscribe` | `SessionSubscribeParams` | `SessionSubscribeResult` | `session.subscribe` | Served |
| `session.unsubscribe` | `SessionUnsubscribeParams` | `Ack` | `session.subscribe` | Served |
| `agent.prompt` | `AgentPromptParams` | `AgentPromptResult` | always | Served |
| `agent.status` | `SessionRef` | `AgentStatusResult` | always | Served |
| `agent.interrupt` | `SessionRef` | `Ack` | `agent.interrupt` | Served |
| `catalog.tools` | none | `ToolCatalogResult` | always | Served |
| `catalog.models` | none | `ModelCatalogResult` | always | Served |
| `config.dump` | none | `ConfigDumpResult` | always | Served |

A call with no params accepts an absent `params`, or `{}`, and treats them alike.

Every call is on the `Engine` trait, `session.subscribe` included.
Its trait form takes one extra argument the wire does not carry: an `Arc<dyn EventSink>`, which is where the carrier wants its pushes delivered.
`SessionSubscribeResult.subscription_id` is what `session.unsubscribe` names, so one caller may hold several subscriptions, and closing one never closes another.
A carrier that drops a connection unsubscribes its sinks; a sink that is gone is dropped by the engine rather than erroring a turn.

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
Two more are durable and staged: `llm/retry` and `llm/retry-started`, written when a provider request failed and the route's policy is trying again (§4.3.2).
A surface renders them raw until it takes them, which is what "the vocabulary grows" means in practice.
`session/start` is the first line of every journal and carries the session header, so listing a cold session reads the log and never a sidecar file.
`assistant/chunk` is the streaming surface.
Raw chunks stay on the log, so a surface replays a stream exactly as it arrived rather than re-deriving it.
There is no separate progress event: progress is `step/start`, the chunks, `tool/call`, `tool/result` and `step/end`, in order.

#### 4.3.1 What `data` carries

`SessionEvent.data` is `Value` because the vocabulary grows.
For the ten types above it is not arbitrary, and this table is the boundary promise.
`SessionEvent::parse()` returns `KnownEvent` for these and `None` for anything else, so a surface gets a compiler-checked path for what it knows and still renders what it does not.
`None` covers two cases and does not distinguish them: a type this build does not know, and a known type whose payload did not match the table.
Both mean the same thing to a caller, which is to render the raw event.

| `type` | `data` |
| --- | --- |
| `session/start` | `session_id`, `provider`, `model`, `max_steps` |
| `turn/start` | `turn` |
| `step/start` | `turn`, `step` |
| `user/message` | `content` |
| `assistant/chunk` | `chunk` (`text` \| `reasoning` \| `tool_call`), plus `delta` for the first two and `call` for the third, plus `turn` and `step` |
| `assistant/message` | `content`, `reasoning`, `tool_calls`, `finish_reason`, `usage` |
| `tool/call` | `id`, `name`, `arguments` |
| `tool/result` | `call_id`, `name`, `ok`, `content` |
| `step/end` | `turn`, `step` |
| `turn/end` | `turn`, `steps`, `stop_reason`, `stop_veto` |

#### 4.3.2 Types that are durable but not yet parsed

`KnownEvent` has no fallback variant, deliberately: `parse()` returns `Option`, so an unknown type is `None` rather than a variant to match.
The cost is that adding a variant is a breaking change for every consumer that matches the enum exhaustively.
A new durable type therefore lands in two steps: this section fixes its payload and the engine starts writing it, and the variant joins `KnownEvent` and the §4.3.1 table in the later version the presentation lane takes.
Until then `parse()` returns `None` for it and a surface renders it raw, which is the behaviour §4.3.1 already promises for a type a build does not know.

| `type` | `data` |
| --- | --- |
| `llm/retry` | `turn`, `step`, `provider`, `code`, `message`, `retry`, `max_retries` (`null` under an unbounded policy), `delay_ms` |
| `llm/retry-started` | `turn`, `step`, `retry` |

`llm/retry` is written before the wait, so a journal records an attempt the process never lived to make.
`llm/retry-started` is written when the wait is over and the request is going out again; between the two, a surface may show the wait counting down.
`retry` counts from one and is the attempt about to be made, not the one that failed.
`code` is the stable failure classification of §4.5, and `message` is the provider's own words.

This step is not a version bump.
`SessionEvent.type` is a free string by §4.3 and the vocabulary is stated there to grow, so a durable type that no boundary struct names changes nothing a peer compiles against.
The second step is the minor bump, because a `KnownEvent` variant is an addition under §5.

**`tool/result.call_id` is the correlation id**, and it equals the `tool/call.id` that asked for it.
A surface pairs a result to its call by that id and never by arrival order, because arrival order stops being pairing order the moment two calls are in flight.
`tool/result` also cites its `tool/call` in `sourceEventSeqs`, so the pairing survives a journal read that starts mid-turn.

The turn's answer is the last `assistant/message.content`, and `turn/end` deliberately does not repeat it.
`TurnSummary.content` is that same text, restated for a caller that did not stream.
A surface reads one or the other, never both, or it renders the answer twice.

Adding a field to one of these payloads is a minor change; removing or renaming one is major.

**`AgentState`**, **`StopReason`** and **`ConfigLayer`** each carry an `Other(String)` fallback.
A surface renders the fallback rather than failing, and that is exactly what lets the engine add a variant in a minor version.

**`SessionInfo`** answers a list view without reading a journal: id, journal path, provider, model, creation time, `last_seq` (`-1` for an empty log), live state, and `title`.
`title` is the session's first user message, truncated by the engine, or `None` when there is none yet.
The engine already holds the journal open when it lists; a picker paging every journal to find that line would be the wrong side of this boundary.

**`TurnSummary`** is the closing shape of one turn.
Most of it is reconstructable from the journal; the summary is the convenience form.
`duration_ms` and `usage` are the two exceptions worth stating.
Elapsed time is derivable from `SessionEvent.time`, and `duration_ms` is only the engine saying it once.
`usage` is **not** derivable from anything else here, so it is named now even where a provider does not report it: both fields are `Option`, and `None` means "this build did not measure it", never zero.
`Usage` uses the provider's own words, `prompt_tokens` and `completion_tokens`, because the same object is what `assistant/message.usage` carries in the journal.

**`ToolDescriptor`**, **`ProviderDescriptor`** and **`ConfigEntry`** are what a help surface, a model picker and `tetanus config` render.
`ProviderDescriptor.available` is false when a provider is registered but its credential is absent, so a picker can grey the entry instead of failing at the first turn.

`ConfigEntry.value` never carries a secret.
A key whose last word is `key`, `secret`, `token`, `password` or `credential` - words split on `.`, `_`, `-` and a capital that starts one, so `api_key`, `apiKey`, `APIKey` and `client-secret` all match while `api_key_env`, `max_tokens` and `monkey` do not - is published with `types::REDACTED`, the string `<redacted>`, in place of the value the document holds.
The entry itself stays, because a surface still has to say that the key is set and which layer set it; only the value is withheld.
The rule reads the name because the engine has no schema for a key it does not settle: a document may hold a provider's credential under whatever name a future adapter reads, and `config.dump` is answered over every carrier, so an unredacted value would reach every connected client.
A surface renders the sentinel as it renders any other value, and must not take it for the setting.

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

A turn a failure ended closes too.
`agent.prompt` answers the §4.5 error instead of a summary, and the closers are on the journal before that answer: `step/end` for the step the failure interrupted, then `turn/end` carrying `stop_reason: "failed"`.
They are the pair §4.4.4 gives crash repair, in that order and with the payloads §4.3.1 fixes, because a reader cannot tell the two journals apart and should not have to.
So §4.6's `running --turn/end--> idle` holds for a failed turn exactly as it holds for a cancelled one, and a reader of the journal never waits for §4.4.4's repair to learn that a turn is over.
The failure itself travels on the error and not on the event: `turn/end` carries the four fields §4.3.1 fixes and nothing more.
`"failed"` is a value of the growable `StopReason` (§7.5), not a new variant.

#### 4.4.3 Asking the user

```text
server -> ui/ask { session_id, questions }   (a request, not a notification)
client -> AskResult { answers }
```

The ask is a server-to-client request because the engine blocks on it: a tool cannot proceed until the human decides.
A client that advertises no `ui.ask` capability is never asked, and the engine denies the underlying action instead of hanging.
A client that advertises the capability and then fails to answer must answer with an error; the engine treats any error as a denial.

#### 4.4.4 Reopening a journal a crash left open

A process that dies mid-turn leaves a journal whose last turn never ended, and possibly a tool call no `tool/result` answers.
`session.create` closes that turn before it answers, so no surface ever sees a session whose history has a dangling call.

The closers are ordinary durable events, written once, at the end of the journal:

```text
session/event tool/result   (one per unanswered call, ok: false)
session/event step/end      (only when a step was open)
session/event turn/end      (stop_reason: "interrupted")
```

A surface reads them exactly as it reads a live turn's, and `SessionInfo.last_seq` counts them.
So a reopened session may report a `last_seq` above the one the surface last saw, and that is the repair, not a lost push.

Each synthesized `tool/result` carries a `code`: `TOOL_NOT_STARTED` when no `tool/call` was ever written for it, and `TOOL_OUTCOME_UNKNOWN` when one was, in which case the result cites that `tool/call` in `sourceEventSeqs`.
The two are worth telling apart in a transcript, because the first is safe to retry and the second is not.

`stop_reason: "interrupted"` is a new value of the growable `StopReason`, and §7.5 already fixes what an old surface does with one.
A balanced journal is untouched, so this is invisible to every session that closed normally.

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

**The mapping from an engine failure to a code is the engine's, and it is published.**
`tetanus_engine::convert::turn_error` maps a failed turn, `tetanus_engine::convert::journal_error` maps a journal fault, and `tetanus_engine::convert::config_error` maps a settings fault.
A surface calls one of them and renders what it returns.
It does not match on an engine error type to derive a code of its own.
Two reasons, and the second is the one that bites.
Which code a failure deserves is a boundary decision, so two surfaces deriving it separately can disagree about the same failure.
And an engine error enum is an internal Rust type with no fallback variant, unlike the wire enums of section 7.5, so a match outside the engine crate stops compiling the day the engine names a new failure.

A settings fault is the one of the three a surface meets before any call: booting an engine on a document is not a request, so nothing has an id to fail yet.
It still takes a code from the table above, because the surface reports it the same way and a script reads the same statuses.
A document that cannot be turned into settings is `Io` carrying its path: the fault is that file, whether the filesystem refused it or its own text did.
A document that was read and holds one value the key does not take is `InvalidParams` carrying the dotted key as `field`, because the key is what the reader edits.

Neither adds a code.
A surface's match on `ErrorCode` is exhaustive on purpose, so a new code is a change both lanes land together, and these two failures need none: the rows above already carry the path, the key and the exit status each case wants.

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
| `tetanus run` | `session.create`, `session.subscribe`, `agent.prompt` |
| `tetanus chat` | `session.create`, `session.subscribe`, then one `agent.prompt` per message typed |
| `tetanus replay <path>` | `session.create` with `path`, then `session.events` |
| `tetanus sessions` | `session.list` |
| `tetanus tools` | `catalog.tools` |
| `tetanus models` | `catalog.models` |
| `tetanus config` | `config.dump` |
| `tetanus serve` | hosts the stdio and WebSocket carriers |
| `tetanus info` | none; build metadata only |

`tetanus chat` is `tetanus run` held open: one `session.create` and one `session.subscribe`, then an `agent.prompt` for each message, all against the same session id.
It needs no call of its own because a conversation is already what a session is: every `agent.prompt` against a session id is a turn on the journal that session has been writing since it was opened (§4.4.1, §4.4.2), and the engine answers each one in the light of the turns already on it.
So nothing about the conversation is held by the surface, and a chat that names the same journal tomorrow is the same conversation.

A journal is addressed by id, never by path, because an id is what every other call takes.
`SessionCreateParams.path` is the bridge: naming a path opens the journal there and returns its `SessionInfo`, with the id read from the journal's own `session/start` line.
So `tetanus replay <path>` and `tetanus run --session <path>` are both `session.create` with a `path`, and neither needs a second call form.
A path with no file yet is created; a path whose file is not a journal is `LogCorrupt`.
A journal outside the server's own directory is reachable by id for as long as the server holds it open, and `session.list` reports only the server's directory.
So a surface that wants a foreign journal back after a restart names its path again; it does not keep the id.
An id is a fact of the journal and not of its file name, so every id `session.list` reports is one that `session.events` and `session.subscribe` resolve, whatever the file holding that journal is called.

Machine-readable output is contract output.
`--json` prints the call's result type verbatim, one JSON object per line, with no added fields and no colour.
A subcommand that streams prints the `SessionEvent` carried by each `SessionEventPush`, as its own line as it arrives, and the call's result as the last line.
The push envelope is not printed: `session_id` is already known to whoever invoked the run, and dropping it makes the stream byte-identical to the journal on disk.
A subcommand that does not stream prints exactly one line.
So a script reads lines until the stream ends and treats the last one as the answer, whichever subcommand it ran.
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

The cases run offline.
Those over the boundary types alone live in `crates/protocol/tests/wire.rs`; those that hold the engine's own output to §4.3.1 live in `crates/engine/tests/contract_events.rs`.
Those that hold a carrier to §4.1 live in `crates/rpc/tests/stdio.rs` and `crates/rpc/tests/websocket.rs`, which drive the same engine double, so a claim proved for one carrier and not the other is a failing case rather than an omission.
Those that hold the published failure mapping to §4.5 live in `crates/engine/tests/faults.rs`.
Those over the interaction and state views (§4.4, §4.6) live in `crates/engine/tests/facade.rs`, `agent.rs`, `subscribe.rs`, `sessions.rs` and `resume.rs`, with the closer synthesis §4.4.4 applies pinned on its own in `crates/turn/tests/upstream_repair.rs`.
Of §4.7, the clauses about which calls a subcommand makes and what it prints are the presentation lane's to verify, in `crates/cli/tests`; the clauses about what those calls do are below.

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
| §4.3.1 every durable payload parses from the journal shape | TC-PROTO-10 |
| §4.3.1 an unknown type parses to `None` and keeps its data | TC-PROTO-11 |
| §4.3.1 `tool/result` names the call it answers | TC-PROTO-12 |
| §4.3.1 every event the engine writes parses, and all ten types appear | TC-CONTRACT-1 |
| §4.3.1 the engine's own `tool/result` names and cites its call | TC-CONTRACT-2 |
| §4.3.1 `TurnSummary.content` restates the last `assistant/message` | TC-CONTRACT-3 |
| §4.3.1 `assistant/chunk` names the step it belongs to | TC-CONTRACT-4 |
| §4.3.1 a chunk keeps its variant | TC-PROTO-13 |
| §4.3.2 a staged type parses to `None` and keeps every documented key | TC-PROTO-16 |
| §4.3 unmeasured facts are absent, never zero | TC-PROTO-14 |
| §4.3 a withheld value is spelled one way and travels as an ordinary value | TC-PROTO-17 |
| §4.3 the engine withholds a secret's value and keeps its entry | TC-CFG-SECRET-1 .. TC-CFG-SECRET-4 |
| §4.1 stdio: one JSON object per line, correlated by id | TC-STDIO-1 |
| §4.1 WebSocket: one JSON object per text frame, correlated by id | TC-WS-1 |
| §4.1 a frame that asks nothing is answered with nothing | TC-STDIO-2, TC-WS-2 |
| §4.1 either push arrives as a notification frame, on either carrier | TC-STDIO-3, TC-WS-3 |
| §4.1 a binary frame is not a frame the WebSocket carrier defines | TC-WS-6 |
| §4.2 a peer that hangs up leaves no subscription open | TC-STDIO-4, TC-WS-4 |
| §4.4.1 the handshake is connection state, one connection at a time | TC-WS-7 |
| §4.4.2 a call is answered while an earlier one is still running | TC-STDIO-5, TC-WS-5 |
| §4.1 the id a server answers when it cannot read one | TC-PROTO-15 |
| §4.5 a credential fault carries `provider` and `env` | TC-FAULT-1 |
| §4.5 a provider that answered carries its status | TC-FAULT-2 |
| §4.5 a call that never reached an answer carries none | TC-FAULT-3 |
| §4.5 a log that refused a chunk is `Internal` | TC-FAULT-4 |
| §4.5 a corrupt journal carries `session_id` and `line` | TC-FAULT-5 |
| §4.5 `Io` carries the path when the caller knows one | TC-FAULT-6 |
| §4.5 every failure a turn can reach has a known code | TC-FAULT-7 |
| §4.5 a document that cannot be booted on is `Io` with its path | TC-FAULT-8 |
| §4.5 a value the key does not take is `InvalidParams` with that key | TC-FAULT-9 |
| §4.2 every call in the method table is served | TC-ENG-3 |
| §4.4.1 a matching major is accepted, and nothing else is | TC-ENG-1, TC-ENG-2 |
| §4.4.2 a prompt runs the documented turn and answers with its summary | TC-AGENT-1 |
| §4.4.2 the pushes a subscriber gets are the journal the turn wrote | TC-AGENT-2 |
| §4.4.3 a reserved call's capability is not advertised | TC-SUB-5 |
| §4.4.4 which closers a journal needs, and which it does not | TC-PORT-REPAIR-1 .. TC-PORT-REPAIR-10 |
| §4.4.4 `session.create` applies them, and `last_seq` counts them | TC-SESS-6 |
| §4.4.4 a journal is repaired once, not once per open | TC-PORT-RESUME-3 |
| §4.6 `agent/status` is pushed on both transitions | TC-AGENT-3 |
| §4.6 `agent.status` reads the live state a missed push lost | TC-AGENT-5 |
| §4.7 naming a path opens that journal, under the id the journal carries | TC-PATH-1, TC-ID-1 |
| §4.7 a path whose file is not a journal is `LogCorrupt` | TC-PATH-3 |
| §4.7 every id `session.list` reports is one `session.events` opens | TC-ID-2 |

## 7. Design rationale

### 7.1 JSON-RPC 2.0, not a generated contract

Upstream's web API is a build artifact of TypeScript decorators (`docs/api-gateway.md`): Typert generates host and client contracts, and identity crosses the wire through a lookup map.
That surface is powerful and undocumented as a protocol, and every upstream release candidate can change it silently.
`docs/PLAN.md` chose option 2 for exactly this reason: our own protocol, fully owned.

JSON-RPC 2.0 was picked over a bespoke framing because it already answers request correlation, one-way notifications, bidirectional calls and a structured error object, and because a presentation surface can drive it from any language without a generator.
The cost is that it says nothing about streams; §4.4.2 answers that by streaming durable session events as notifications instead of inventing a stream type.

### 7.1.1 One sink, three carriers

The first draft left `session.subscribe` off the `Engine` trait, on the reasoning that a subscription binds to a connection and an in-process caller has none.
The presentation lane rejected that in review, correctly: it left the in-process renderer with no way to see a chunk arrive except by importing `tetanus-core` and `tetanus-turn`, which is the lane boundary §3 draws.

`EventSink` is the fix. The subscription binds to a sink rather than to a connection, the carrier supplies the sink, and the wire never carries it.
The alternative was a second in-process-only call, which would have meant two code paths to keep in step and a renderer that behaves differently depending on how it was launched.

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
A fallback variant answers an unknown value, not an added variant, which is why section 3 rule 4 asks for an open match as well.

### 7.6 Wire types duplicated, not re-exported

`SessionEvent` exists in `tetanus-session` and again in `tetanus-protocol`.
Re-exporting would drag `tokio`, `tetanus-core` and the event bus into every consumer of the contract, and would tie the wire shape to an internal type that the engine must stay free to refactor.
TC-PROTO-5 pins the wire shape to the journal line, and the engine-side conversion is covered where it lands.

## 8. Changelog

Every boundary change adds a row here, in its own pull request.

| Version | Change |
| --- | --- |
| 1.0 | First contract: envelope, error codes and exit statuses, session and agent calls, tool, model and config catalogues, `session/event` and `agent/status` pushes, `ui/ask` reserved. |
| 1.0 | Reconciles the presentation lane's consumer review, before 1.0 is served: `EventSink` puts `session.subscribe` on the `Engine` trait so every carrier feeds one renderer (§4.1, §4.2); §4.3.1 fixes the `data` payload of each durable type and `SessionEvent::parse()` makes it compiler-checked; `SessionCreateParams.path` addresses a journal by path (§4.7); `--json` streaming is stated (§4.7); `TurnSummary.duration_ms` and `usage`, and `SessionInfo.title`, are named. |
| 1.0 | Names the id a server answers with when it cannot read one (§4.1): `rpc::Id::Null`, serialized as JSON `null`, for a frame that is not JSON, is not a request, or is a batch array. No carrier existed when this landed, so no peer had observed the gap. |
| 1.0 | Settles which type a streaming `--json` subcommand prints (§4.7, issue #56): the `SessionEvent` out of the push, not the `SessionEventPush` envelope. Wording only; it is what the presentation lane already shipped, and it keeps the stream byte-identical to the journal. |
| 1.0 | Names what `session.create` does with a journal a crash left mid-turn (§4.4.4): it appends the missing `tool/result`, `step/end` and `turn/end` closers before answering, so `last_seq` may jump and `stop_reason: "interrupted"` may appear. No type changes: `StopReason` is growable by §7.5, and the closers use payloads §4.3.1 already fixes. |
| 1.0 | States the guarantee behind a session id (§4.7, issue #67): an id is a fact of the journal's `session/start` line and not of its file name, so every id `session.list` reports resolves for the other `session.*` calls. Wording only, and no type changes: it names which of two readings the engine is held to, and the engine defect that resolved the other way is fixed on its own. |
| 1.0 | No boundary change. Records in §6 that §4.3.1 is now verified against the engine's own output, not only against the boundary type: `crates/engine/tests/contract_events.rs` runs a real turn and parses every event it wrote (TC-CONTRACT-1..4). A renamed durable field used to fail no test. |
| 1.0 | No boundary change. Records in §6 that §4.1's "one contract, three carriers" is now verified against two of them: `crates/rpc/tests/stdio.rs` and `crates/rpc/tests/websocket.rs` assert the same claims against the same engine double (TC-STDIO-1..5, TC-WS-1..7). The WebSocket carrier is served; no subcommand hosts it yet. |
| 1.0 | Names who maps an engine failure to a code (§4.5): the engine does, in `tetanus_engine::convert::turn_error` and `convert::journal_error`, which this change publishes. A surface must not match on an engine error type to derive a code of its own, because an engine error enum has no fallback variant and a surface that matches one stops compiling the day the engine names a new failure. No wire types change. Two mapping fixes travel with it: `Io` now carries the `path` the table already asked for, and a session log that refused a chunk is `Internal` rather than `ProviderError`, since nothing about the provider was wrong and retrying it cannot help. The presentation lane's own copy of the mapping in `crates/cli` is redundant from this version; removing it is that lane's change. |
| 1.0 | Publishes two durable types the engine is about to write (§4.3.2): `llm/retry` before a policy's wait and `llm/retry-started` when the wait is over, so a surface can say a request is being retried instead of showing a stalled turn. No type changes, no version bump and nothing to recompile: `type` is a free string by §4.3, and the two are deliberately not `KnownEvent` variants, because that enum has no fallback and growing it stops a consumer's build. §4.3.2 states the two-step rule that follows from that; the presentation lane decides when to take the variants, and that step is the minor bump. TC-PROTO-16 pins the staged behaviour. |
| 1.0 | No boundary change, and no type changes: states in section 3 that a surface matches the wire types with a fallback arm, and never matches an engine enum such as `tetanus_turn::StopReason` (issue #142). Section 4.5 said this for engine error types only, so a growable enum that is not an error was left unstated, and every added variant is a build break in the consuming lane rather than the minor change section 5 promises. `KnownEvent` is named too: an open match on it is what would let section 4.3.2's two steps become one. The rule is a promise here rather than a compiler error, because `#[non_exhaustive]` on those enums would fail the build of a surface that has not adopted it yet; the marker lands with the adoption, in its own row. |
| 1.0 | Adds a ninth subcommand to §4.7: `tetanus chat`, an interactive conversation defined as `session.create`, `session.subscribe`, then one `agent.prompt` per message typed. No new calls, no new types, no version bump - the table is the closed list of what a subcommand may call, so a subcommand that calls nothing new still lands here to keep that list true. Section 4.7 states why a conversation needs no call of its own: a session is already the conversation, so many turns typed into one is the mechanism two `tetanus run --session <path>` invocations already use. |
| 1.0 | Publishes the third failure mapping (§4.5): `tetanus_engine::convert::config_error`, for the settings document a surface boots on. No code is added and no type changes - a document that cannot be turned into settings is `Io` with its path, and one value its key does not take is `InvalidParams` with that key as `field`. The rule against a surface deriving its own code applies to `tetanus_config::ConfigError` for the reason it applies to the others: it is an internal enum with no fallback, and it grew a variant in the change that gave the engine a settings boot. Issue #157 asked which code a settings fault takes; this is the answer, so the presentation lane's wiring of `tetanus_engine::boot` needs no decision of its own. |
| 1.0 | States that a turn a failure ended still closes on the journal (§4.4.2): the step the failure interrupted gets its `step/end` and the turn gets a `turn/end` with `stop_reason: "failed"`, both written before `agent.prompt` answers its §4.5 error. Until now a failed turn left `turn/start` unbalanced, so §4.6's state machine was true only for a turn that succeeded and a reader had to wait for §4.4.4's repair on the next open to learn the turn was over. No type changes: `"failed"` is a value of the growable `StopReason` by §7.5, and the closer uses the payload §4.3.1 already fixes. The failure stays on the error object, where §4.5 puts it; the engine change that writes the closer lands on its own. |
| 1.0 | No boundary change, and no type changes. Completes §6 so every clause a case already fixes names it: §4.4.1, §4.4.2, §4.4.3, §4.4.4, §4.6 and the engine-side half of §4.7 had cases but no row, which is the one gap the table cannot show, since a clause with no row reads the same whether it is unverified or only unrecorded. §4.4.3 stays reserved, and its row is the promise that goes with that: the capability behind an unserved call is not advertised. §6's preamble now also says which of §4.7 the presentation lane verifies, so a reviewer looking for the missing cases looks in the right suite. |
| 1.0 | States that `config.dump` never publishes a secret's value (§4.3): a key whose last word names a credential is dumped with `types::REDACTED` in place of what the document holds, and keeps its key and its layer, so a surface can still show that it is set. One added constant, no type change, no version bump - the entry shape is what it was, and a client that does nothing new reads the sentinel as the string it is. The rule is by name because the engine has no schema for a key it does not settle. Until now every key a document held was echoed verbatim, to `tetanus config` and to any WebSocket client alike, so a credential written into the document was published to whoever was connected. The engine change that applies this lands beside it, with the cases §6 names (TC-CFG-SECRET-1..4); the presentation lane's own temporary copy of the dump in `crates/cli` (issue #157) is that lane's to fix. |
