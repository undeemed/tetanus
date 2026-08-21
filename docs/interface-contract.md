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
| `session.fork` | `SessionForkParams` | `SessionInfo` | `session.fork` | Served |
| `session.subscribe` | `SessionSubscribeParams` | `SessionSubscribeResult` | `session.subscribe` | Served |
| `session.unsubscribe` | `SessionUnsubscribeParams` | `Ack` | `session.subscribe` | Served |
| `agent.prompt` | `AgentPromptParams` | `AgentPromptResult` | always | Served |
| `agent.status` | `SessionRef` | `AgentStatusResult` | always | Served |
| `agent.interrupt` | `SessionRef` | `Ack` | `agent.interrupt` | Served |
| `catalog.tools` | none | `ToolCatalogResult` | always | Served |
| `catalog.models` | none | `ModelCatalogResult` | always | Served |
| `config.dump` | none | `ConfigDumpResult` | always | Served |
| `approval.set` | `ApprovalSetParams` | `Ack` | `approval.set` | Reserved |
| `agent.steer` | `AgentSteerParams` | `AgentSteerResult` | `agent.steer` | Reserved |

A call with no params accepts an absent `params`, or `{}`, and treats them alike.

A reserved call is routed like any other, so it answers `NotImplemented` (`-32001`) and never `MethodNotFound` (`-32601`).
The two are a whole decision apart for a caller: §4.5 exits 3 on the first, meaning this build rather than this call, and 2 on the second, meaning the caller is wrong.
In Rust, `Reserved` is a default body on the `Engine` trait method that returns that error, so a shape can be frozen and compiled against before any build serves it.
The slice that serves the call deletes that body, and §7.4's compile error for every implementor comes back with it.

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
| `ui/approve` | request | `ApproveParams` | `ApproveResult` | Reserved, capability `ui.approve` |

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
Six more are durable and staged: `llm/retry` and `llm/retry-started`, written when a provider request failed and the route's policy is trying again, `approval/asked`, `approval/decided` and `approval/policy`, the audit of a decision about whether a tool may run, and `context/snapshot`, the live facts a turn told the model about the world it is running in (§4.3.2).
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
| `session/start` | `session_id`, `provider`, `model`, `max_steps`, plus `parent_session` and `fork_seq` on a journal that was forked (§4.4.6), plus `cwd`, `spawned_by` and `depth` where they apply (§4.4.9) |
| `turn/start` | `turn` |
| `step/start` | `turn`, `step` |
| `user/message` | `content` |
| `assistant/chunk` | `chunk` (`text` \| `reasoning` \| `tool_call`), plus `delta` for the first two and `call` for the third, plus `turn` and `step` |
| `assistant/message` | `content`, `reasoning`, `tool_calls`, `finish_reason`, `usage` |
| `tool/call` | `id`, `name`, `arguments` |
| `tool/result` | `call_id`, `name`, `ok`, `content`, plus `code` on a result the engine synthesized rather than ran (§4.4.4) |
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
| `approval/asked` | `id`, `tool_name`, plus `call_id` and `reason` when the asker had them |
| `approval/decided` | `id`, `outcome` |
| `approval/policy` | `policy` |
| `context/snapshot` | `turn`, `parts` (each `name` and `text`) |
| `user/steer` | `content`, `turn`, `taken` |

`llm/retry` is written before the wait, so a journal records an attempt the process never lived to make.
`llm/retry-started` is written when the wait is over and the request is going out again; between the two, a surface may show the wait counting down.
`retry` counts from one and is the attempt about to be made, not the one that failed.
`code` is the stable failure classification of §4.5, and `message` is the provider's own words.

The three `approval/*` types are §4.4.7's audit.
`approval/asked` and `approval/decided` are one pair sharing an `id`, and `approval/policy` is the durable form of a policy switch: the last one on the journal is the session's override.
`outcome` is one of §4.4.7's four words and `policy` one of its two, both spelled exactly as the wire enums spell them.
They carry no `turn` or `step`, as `tool/call` and `tool/result` carry none: their place is their position between the boundaries of the step that asked.

`context/snapshot` is §4.4.8's record of what a turn told the model about the world outside the conversation - the date, the working directory, the branch it is on.
It carries the parts rather than the rendered text, joined the way §4.4.8 fixes, so a surface can show which provider contributed what and a reader can still reconstruct exactly what the model saw.
It names its `turn`, because unlike the approval pair it belongs to the turn rather than to a moment inside a step.

This step is not a version bump.
`SessionEvent.type` is a free string by §4.3 and the vocabulary is stated there to grow, so a durable type that no boundary struct names changes nothing a peer compiles against.
The second step is the minor bump, because a `KnownEvent` variant is an addition under §5.

**`tool/result.code` is present only on a result nobody ran.**
A call the engine dispatched has an outcome, and the outcome is `ok` and `content`.
A result the engine *synthesized* - crash repair closing a call that was interrupted (§4.4.4), or a call refused before it ran (§4.4.7) - has no outcome to report, so it carries a code saying why there is none.
The vocabulary grows with the reasons, so a surface reads an unknown code as "not run, for a reason this build does not know" rather than failing.

`KnownEvent::ToolResult` does **not** carry it yet, and that is a gap rather than a decision.
`parse()` drops the field, so a surface on the typed path cannot today tell `TOOL_NOT_STARTED` from `TOOL_OUTCOME_UNKNOWN` - the distinction §4.4.4 calls load-bearing, because the first is safe to retry and the second is not.
The field is on the journal and on `SessionEvent.data`, so nothing is lost to a surface willing to read it there; what is missing is the compiler-checked path.
It is deferred for the reason below rather than added here, and it lands in the version the presentation lane takes.

**`tool/result.call_id` is the correlation id**, and it equals the `tool/call.id` that asked for it.
A surface pairs a result to its call by that id and never by arrival order, because arrival order stops being pairing order the moment two calls are in flight.
`tool/result` also cites its `tool/call` in `sourceEventSeqs`, so the pairing survives a journal read that starts mid-turn.

The turn's answer is the last `assistant/message.content`, and `turn/end` deliberately does not repeat it.
`TurnSummary.content` is that same text, restated for a caller that did not stream.
A surface reads one or the other, never both, or it renders the answer twice.

Adding a field to one of these payloads is a minor change; removing or renaming one is major.

**`AgentState`**, **`StopReason`**, **`ConfigLayer`**, **`ApprovalOutcome`** and **`ApprovalPolicy`** each carry an `Other(String)` fallback.
A surface renders the fallback rather than failing, and that is exactly what lets the engine add a variant in a minor version.

The two approval enums are the first whose fallback is read rather than rendered, so §4.4.7 fixes what reading one means.
An `ApprovalOutcome` the engine does not know denies, because a word it cannot interpret is not a grant.
An `ApprovalPolicy` it does not know is `InvalidParams`, because a policy is set by a caller that could have named one of the two.

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
A value is withheld - published as `types::REDACTED`, the string `<redacted>`, in place of what the document holds - when **either** of two rules says so.

**The schema, where there is one.**
The engine settles a known set of keys and knows what each is for, so for those the schema is the authority: a key declared to hold a credential is withheld whatever it is called.
This is what catches a credential whose name says nothing, `llm.providers.acme.authorization` or plain `llm.providers.acme.a`, which no rule reading the name could find.

**The name, for everything else.**
A key whose last word is `key`, `secret`, `token`, `password` or `credential` - words split on `.`, `_`, `-` and a capital that starts one, so `api_key`, `apiKey`, `APIKey` and `client-secret` all match while `api_key_env`, `max_tokens` and `monkey` do not - is withheld too.
A document may hold a credential under whatever name a future adapter reads, and the engine has no schema for a key it does not settle, so without this every unsettled key would be published in full.

**The two compose by union, never by override, and that direction is the whole point.**
A schema that could *un*-mark a key would mean adding a key to the schema is a way to start publishing it, and the mistake would be silent and permanent.
Each rule alone fails one way: a schema misses what it does not describe, a name rule misses what is not named like a secret. Their union fails safe, and the cost - a non-secret key called `monkey_token` withheld for nothing - is a value a user can still see in their own file.

The entry itself stays, because a surface still has to say that the key is set and which layer set it; only the value is withheld.
`config.dump` is answered over every carrier, so an unredacted value would reach every connected client.

**A surface must not read the sentinel as proof.**
Nothing distinguishes a withheld value from a document that literally contains the string `<redacted>`, and a surface that treated the sentinel as "this is a secret" would mislabel the second.
The honest signal is a flag on the entry saying the value was withheld, and it is deliberately not added here: `ConfigEntry` is a type the presentation lane constructs, so by §5 the field is a change both lanes land together, and it lands in the version that lane takes it.
Until then a surface renders the sentinel as it renders any other value, and must not take it for the setting.

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
That is still true, and §4.4.10 is the promised separate call for a caller that wants the follow-up taken rather than refused.

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

A turn the provider cut off at its output cap closes too, and says so.
When a completion ends because the model reached the cap on what it may write, rather than because it had finished writing, `turn/end` carries `stop_reason: "max-tokens"`.
`agent.prompt` still answers a summary and not an error: the turn produced an answer, and what the reason adds is that the answer is incomplete.
A surface that renders it as an ordinary end tells the reader that a sentence the model never finished is the whole reply.

The reason belongs to the turn, not to the step that was cut off.
A turn one of whose steps hit the cap ends `max-tokens` even when a later step completed, because the answer the reader is holding is still missing the part that was cut.
It does not carry into the next turn: a turn whose own steps all completed ends `natural`, whatever happened in the turn before it.

A step the cap cut off dispatches no tool calls.
A completion that stopped mid-write can stop in the middle of a call's arguments, so what the model asked for is not known and cannot be repaired by guessing.
The step ends with its `assistant/message` and no `tool/call`, and the turn ends there.

Those calls are not written to the `assistant/message` either.
The event carries the §4.3.1 fields it always carries, with `tool_calls` empty and `finish_reason` still the provider's own word for the cap, so a reader can tell a cut-off answer from a finished one.
A call kept there would be a call no `tool/result` ever answers, and the message a client derives from the journal would ask a provider for a result it cannot supply - the next request on that session would be refused, not merely incomplete.
What the provider did send is still on the `assistant/chunk` events the message cites, so nothing is lost to a reader who wants the raw stream.

`"max-tokens"` is a value of the growable `StopReason` (§7.5), not a new variant, so no wire type changes.
The engine names it as a reason of its own, and §7.6's mapping carries it across as the fallback, the way §4.4.4's `"interrupted"` already travels.

#### 4.4.3 Asking the user

```text
server -> ui/ask { session_id, questions }   (a request, not a notification)
client -> AskResult { answers }
```

The ask is a server-to-client request because the engine blocks on it: a tool cannot proceed until the human decides.
A client that advertises no `ui.ask` capability is never asked, and the engine denies the underlying action instead of hanging.
A client that advertises the capability and then fails to answer must answer with an error; the engine treats any error as a denial.

A turn a guard stopped closes too, and says which guard.
A **guard** is a bound the deployment set on a turn rather than on a request: how long the whole turn may take, and how many times the model may do the same thing.
`turn/end` carries `stop_reason: "timed-out"` or `"repeated"`, and `agent.prompt` answers a summary rather than an error - the turn produced whatever it produced, and what the reason adds is why it stopped short.

The two are separate reasons because they need separate answers.
`"timed-out"` says the work was not finished in the budget, and the usual response is a bigger budget or a smaller task.
`"repeated"` says the model was looping - the same tool with the same arguments, over and over - and a bigger budget makes that strictly worse.
Collapsing them into one reason would leave a reader unable to tell "this needs longer" from "longer will not help", which is the only decision the reason is for.

A guard stops the turn at a step boundary, like `agent.interrupt`, and for the same reason: a step already dispatched has already had its effect.
So a guarded turn is a whole turn with its journal balanced, not a truncated one, and §4.6's `running --turn/end--> idle` holds unchanged.

Both are values of the growable `StopReason` (§7.5), not new variants, so no wire type changes and an older surface renders them through its fallback.
Neither is an error and neither adds a code: a bound the deployment chose being reached is the bound working.

#### 4.4.4 Reopening a journal a crash left open

A process that dies mid-turn leaves a journal whose last turn never ended, and possibly a tool call no `tool/result` answers.
`session.create` closes that turn before it answers, so no surface ever sees a session whose history has a dangling call.

The closers are ordinary durable events, written once, at the end of the journal:

```text
session/event approval/decided  (one per unanswered ask, outcome: "cancelled")
session/event tool/result       (one per unanswered call, ok: false)
session/event step/end          (only when a step was open)
session/event turn/end          (stop_reason: "interrupted")
```

A surface reads them exactly as it reads a live turn's, and `SessionInfo.last_seq` counts them.
So a reopened session may report a `last_seq` above the one the surface last saw, and that is the repair, not a lost push.

Each synthesized `tool/result` carries a `code`: `TOOL_NOT_STARTED` when no `tool/call` was ever written for it, and `TOOL_OUTCOME_UNKNOWN` when one was, in which case the result cites that `tool/call` in `sourceEventSeqs`.
The two are worth telling apart in a transcript, because the first is safe to retry and the second is not.

A synthesized `approval/decided` closes an `approval/asked` the crash caught mid-question, and it reads `cancelled` rather than `unavailable`: nobody was found to be missing, the process holding the question died.
It comes first, before the `tool/result` of the call it was about, because that is the order a live turn writes them in: a decision precedes the call it decides.
An ask that was already decided is untouched, exactly as an answered `tool/call` is.

This closer is why §4.4.7 requires an open turn to ask at all.
The turn is the unit repair closes: `session.create` finds the last `turn/start` with no `turn/end` and balances what it opened.
An `approval/asked` written between turns is inside nothing, so no repair would ever reach it, and a journal would carry a question with no answer for the rest of its life.

`stop_reason: "interrupted"` is a new value of the growable `StopReason`, and §7.5 already fixes what an old surface does with one.
A balanced journal is untouched, so this is invisible to every session that closed normally.

#### 4.4.5 Reading a journal: `from_seq`, `limit`, and the boundary

`session.events` and `session.subscribe` both take a `from_seq`, and it means the same thing on each: a seq, not a count, and inclusive.
The first event a caller receives is the one whose `seq` equals it.
`session.subscribe` may omit it, which asks for live events only and replays nothing.

A `from_seq` past the end of the journal is a resync that had nothing to catch up on, not a fault.
`session.events` answers an empty page with `eof: true` and a `next_seq` at the tail, so a caller that asks from a seq the journal never reached is told where the journal actually ends.
`session.subscribe` replays nothing and delivers live events from there.

`limit` is a page size the server clamps to its own maximum, which is 500.
Zero is read as absent, so `limit: 0` and no limit both name that maximum.
A page of no events is never what a caller meant, and answering one would stall a pager: `next_seq` would not advance and `eof` would stay false, so the loop would never end.
`next_seq` is the `from_seq` of the next page, and `eof` says this page reached the end; a caller that wants the whole journal pages until `eof`.

`SessionSubscribeResult.last_seq` is the seq the subscription starts after (`-1` for a journal with no events).
Every event with a higher seq arrives as a `session/event` push, so a caller needs no second call to find the boundary between what it was given and what it will be sent.
Replay and live delivery join at that boundary with no gap and no repeat, whatever is appended while the replay runs.

A push carries the session it belongs to, and reaches only the subscriptions on that session.
Both frames follow the rule: one connection may hold subscriptions on several sessions and never sees another session's `session/event` or `agent/status`.

#### 4.4.6 Forking a session

`session.fork` opens a new journal seeded with a prefix of another one's, so a caller can take a conversation a second way without losing the first.

The child is a copy and not a reference.
Nothing appended to either journal after the fork reaches the other, and the parent is not written to at all: a fork is a read of the parent and a write of the child.

The child's journal is the inherited prefix with one line replaced:

```text
seq 0                 session/start   the child's own header, carrying parent_session and fork_seq
seq 1 .. fork_seq     the parent's events, copied as they stand, keeping their own seq
seq fork_seq+1 ..     the child's own work
```

The parent's `session/start` is the one line that is not copied, because the child's takes its place, one line for one line.
That is what keeps §4.3's rule that `seq` equals the index of the line true of a forked journal, and it is why the copied events need no rewriting: a `sourceEventSeqs` in the prefix still names the events it named, since nothing ever cites seq 0.

`fork_seq` is the last parent seq the child inherited, inclusive.
It is `through_seq` when the caller named one and the parent's last seq when it did not.
A parent that holds only its header is forked into a child that inherits nothing and reports `fork_seq: 0`, and the arithmetic is the same either way: the child's own first event is always the one after `fork_seq`.

The child inherits the parent's provider, model and `max_steps` along with its events, because the history it starts from was produced under them.
So `session.fork` takes no route parameters, and a caller that wants the same history under another model has no call for that in this version.

**A boundary must be a closed one.**
The inherited prefix may end on a between-turn event and must not end inside an open turn.
The rule is stated over the log rather than over live state: the last `turn/start` or `turn/end` at or before the boundary decides, and a `turn/start` means the boundary is inside that turn.
A child whose first inherited fact is half a turn would need §4.4.4's repair to close a turn that never ran on it, and would offer a provider a dangling tool call as its own history.

What the call refuses, and which of §4.5's codes each refusal takes:

| Refused | Code | `data` |
| --- | --- | --- |
| the source is not a session this server can open | `SessionNotFound` | `{ session_id }` |
| `through_seq` is past the parent's last event | `InvalidParams` | `{ field: "through_seq" }` |
| the boundary is inside an open turn | `InvalidParams` | `{ field: "through_seq" }` |
| `child_session_id` already has a journal | `InvalidParams` | `{ field: "child_session_id" }` |
| `child_session_id` is not 1 to 128 characters of `[A-Za-z0-9._-]` | `InvalidParams` | `{ field: "child_session_id" }` |

The open-turn refusal names `through_seq` even when the caller omitted it, because naming an earlier one is the fix.

A source whose seqs are not contiguous has no fork boundary to argue about, and takes the answer any read of it takes: `LogCorrupt`, naming the line, from the read that fetches the journal.
A source this process holds open cannot reach that state at all, because each seq is assigned from the log's own length as the line is written.

A turn in flight on the source is not a refusal by itself.
A journal is append-only, so a prefix of it is stable while it grows, and a caller that names a closed boundary gets exactly the child it asked for however busy the parent is.
The one boundary a running turn moves is the default one - the parent's last event - and that is the case the open-turn rule already catches.

A `child_session_id` that already has a journal is refused rather than reopened, which is the one place this call differs from `session.create` (§4.7).
Reopening an id is what makes a session resumable; a fork that reopened one would append a second history to a journal that already holds one.

The result is the child's `SessionInfo`, and the child is open when it is answered, so `agent.prompt` may run on it with no further call.
Lineage is read from the child's `session/start` line and is deliberately not repeated on `SessionInfo`; §5 says why an added field on a type the other lane constructs is not the free addition it looks like.
No subcommand calls `session.fork`: §4.7's table is the closed list of what a subcommand may call, and a row joins it when the presentation lane takes the affordance.

#### 4.4.7 Deciding whether a tool may run

Some tools do something a session cannot take back.
Before one of those runs, the engine asks, and the answer decides.

```text
session/event approval/asked     { id, tool_name, call_id, reason }
server -> ui/approve { session_id, request_id, tool_name, call_id, reason }   (a request)
client -> ApproveResult { outcome }
session/event approval/decided   { id, outcome }
```

`request_id` on the wire is the `id` on the journal, so a surface can pair the prompt it is showing with the audit line, and `call_id` is the `tool/call.id` it already streamed.
`reason` is the asker's own words for why it is asking, and it is text for a human, not a code to match on.

**Four outcomes, and one of them is a grant.**

| `outcome` | Means |
| --- | --- |
| `allowed-once` | run this call |
| `rejected` | a decision not to run it |
| `cancelled` | the question was withdrawn before it was answered |
| `unavailable` | nobody could answer it |

A grant is for the one call it was asked about.
It is not a rule, not a session setting and not a grant for the same tool later; the next call of the same tool asks again.
Anything a caller wants to be remembered is a policy, which is the other half of this section.

**The seam fails closed.**
Every way of not getting an answer denies:

- a client that advertises no `ui.approve` capability is never asked, and the ask settles `unavailable` without a frame going out;
- a client that is asked and answers with a JSON-RPC error settles `unavailable`;
- a client that answers with an `outcome` outside the four words settles `unavailable`, because §4.3's fallback is read here and a word the engine cannot interpret is not a grant;
- a connection that drops with the question outstanding settles `unavailable`.

The difference between `rejected` and `unavailable` is who is speaking: the first is a decision, the second is its absence.
They deny alike, and they are told apart on the journal so a transcript can say whether a human refused or nobody was there.

**An interrupt withdraws the question.**
`agent.interrupt` while an ask is outstanding settles it `cancelled` at once, rather than waiting for an answer that a stopped turn would not use.
An answer that arrives after that is discarded, and never reopens a decision the journal has already recorded.
This is the one place an interrupt takes effect inside a step rather than at its boundary (§4.4.2), and it is not a change to that rule: the step is not cut short, the question is, and the step then proceeds with the denial.

**The policy decides before anyone is asked.**

| `policy` | Means |
| --- | --- |
| `ask` | put the question to the client, under the rules above |
| `never` | do not put it to anyone: every ask settles `rejected` |

`never` is the unattended stance, and its point is that the answer is knowable without a human: a run in CI neither hangs nor waits for a client that will not answer.
It settles `rejected` and not `unavailable`, because a deployment that chose it did decide.

A session's policy is the last `approval/policy` on its journal, and the deployment's `approval.policy` setting when the journal holds none.
The fold is the whole state, so a resumed session is under the policy it was under, with nothing to replay but the log itself.
`approval.set` writes that event; it is the only thing that does, and a caller reads the policy back by folding the events it already receives rather than by a call of its own.

Setting the policy a session is already under writes nothing, so a surface may send it idempotently.
A policy outside the two words is `InvalidParams` naming `policy`, and the journal is not written.

**The audit pair is exactly one to one.**
Every ask appends `approval/asked` before the question goes out, and exactly one `approval/decided` with the same `id` once the outcome is known - including the outcomes nobody was asked for, so a `never` policy still leaves the pair on the journal.
An `id` is fresh per ask and is never reused, so two calls of the same tool in one step are two pairs.
The pair is inside the open turn, for the reason §4.4.4 gives: the turn is what repair closes, and a question outside one could never be closed.
Asking with no turn open is `Internal`, and nothing is appended.

**A denied call is a `tool/result`, not an error.**
The call is not dispatched, and the step gets a `tool/result` with `ok: false` whose `content` says the call was not permitted.
§4.5 already fixes this shape: a binding rejection the model reads is not a failure of the call the surface made, so `agent.prompt` still answers a summary and the turn ends normally.
The model is told, so it can do something else rather than wait on a result that is not coming.

**Which tools ask is not on `ToolDescriptor` in this version.**
A surface learns it from the ask.
`ToolDescriptor` is a type the presentation lane constructs in its own cases, so §5's rule applies: an added field is minor on the wire and a build break in the lane that builds the value.
The field lands when both lanes take it, in its own row here - the same deferral §4.4.6 makes for a forked session's lineage.

#### 4.4.8 Telling the model where it is

Some of what a model needs is not in the conversation and is not stable: today's date, the working directory, the branch, whether the sandbox is on.
A **runtime context** is that, gathered once per turn and written to the journal as `context/snapshot`.

```text
session/event turn/start
session/event context/snapshot   { turn, parts: [ { name, text }, ... ] }
session/event step/start
```

**It is a user message, not part of the system prompt, and that is the whole design.**
A provider caches a prompt by its longest stable prefix.
The system prompt is the same on every turn of a session, so it caches; a sentence saying what time it is changes every turn, and putting it there would invalidate the cached prefix on every request of every session.
Carrying it after the retained history instead leaves the prefix untouched, and costs a message.

**Only the newest snapshot is history.**
A turn writes one, so a long session accumulates them, and yesterday's date is worse than no date.
When history is derived, the last `context/snapshot` on the journal becomes a `user` message and every earlier one is skipped.
They stay on the journal, because the journal records what happened and a reader may want to know what the model was told at the time; they simply do not travel again.

**The parts are the record; the joining rule is here.**
`parts` is an ordered list of `name` and `text`.
The message the model reads is the parts whose `text` is non-empty, joined with a blank line between them, in the order the list gives - the same rule §4.3 already fixes for prompt sections, because two joining rules would be one too many.
A snapshot whose parts are all empty is not written at all, so a deployment that configures no providers pays nothing.

Carrying the parts rather than the rendered text is deliberate.
The rendering is reproducible from them by the rule above, so nothing is lost, and a surface that wants to show which provider said what has it.
It is the same choice §4.3 makes when `turn/end` declines to repeat the answer that is already on the last `assistant/message`.

**A snapshot is a fact, not a promise about the future.**
It says what was true when the turn started.
Nothing re-reads it mid-turn, so a step that runs for ten minutes is working from the time the turn began, and a tool that changes the working directory does not retroactively change what the model was told.

**Ordering is the provider's, not the reader's.**
`parts` arrives in the order the engine assembled it and a surface renders it in that order.
There is no priority field on the wire: which provider comes first is a deployment's configuration, settled before the snapshot is written, and putting an order on the durable record would let two readers disagree about the text the model actually saw.

This adds no call and no capability.
A runtime context is contributed inside the engine, and what crosses this boundary is only the record of what was contributed.
#### 4.4.9 What a journal says about where it came from

A journal is read long after the process that wrote it, often on another machine.
Four facts about its origin are worth writing down once, on `session/start`, rather than inferring later or losing.

| Field | Says |
| --- | --- |
| `cwd` | the working directory the session was opened in |
| `parent_session` | the session this journal's history was **copied from** (§4.4.6) |
| `spawned_by` | the session that **started** this one as a subagent |
| `depth` | how many levels of delegation deep this session is; absent means none |

All four are optional, so every journal written before this parses unchanged.

**`spawned_by` is not `parent_session`, and merging them would lose the distinction that matters.**
A fork is a copy: the child begins holding the parent's history and is a second way of continuing one conversation.
A subagent is a different conversation that another one asked for: it shares no history, and the two run at the same time.
A reader that cannot tell them apart cannot answer either "what else came out of this conversation" or "why does this session exist", and those are the two questions lineage is for.
A session can carry both - a fork of a subagent's journal is still a subagent's work - which is the case that rules out one field with a kind beside it.

**`depth` counts delegation, and it is what bounds it.**
A root session has none. A subagent it starts is at one, and one that subagent starts is at two.
It is durable rather than held in memory because the bound has to survive a resume: a subagent whose harness restarted must not come back believing it is a root session and free to delegate again.
What the limit is, and what happens at it, is a deployment's business and not this contract's; what is fixed here is that the number is on the journal and counts levels rather than siblings.

**`cwd` is where the session was opened, not where it is now.**
A tool may change directory; the header is not rewritten, and a reader must not take it for the current state.
It is here because a journal full of relative paths is unreadable without it, and because "it worked on my machine" is usually a question about this field.

**A fork inherits the origin facts it is a copy of.**
§4.4.6 says the child's `session/start` replaces the parent's line for line, so the child writes its own header - and into it go the parent's `spawned_by`, `depth` and `cwd`, because a fork is the same work taken a second way and not a new piece of work.
Its `parent_session` and `fork_seq` are its own, naming the journal it was copied from.

Adding these is a minor change on the wire, and it happens also to be a safe one in Rust: the presentation lane matches `KnownEvent::SessionStart` with a rest pattern, so the added fields do not break its build.
That is worth stating rather than assuming, because §5's rule is about types the other lane *constructs*, and the next addition to this payload has to check again rather than inherit the conclusion.

None of this adds a call, and none of it appears on `SessionInfo`, for the reason §4.4.6 gives: a field added to a type the other lane builds is not the free addition it looks like, and lineage is read from the journal line.
#### 4.4.10 Saying something while a turn is running

`agent.prompt` refuses a second prompt with `SessionBusy`, and that is right for a caller that meant to start a turn.
It is wrong for the commonest thing a person actually does, which is to notice something mid-answer and say so.
`agent.steer` is that: a message handed to the turn already running.

```text
client -> agent.steer { session_id, content }
server -> AgentSteerResult { turn, taken_at_step }

  server -> session/event user/steer
```

**It joins the running turn; it does not start one.**
The message is put in the turn's inbox and claimed at the next step boundary, so the model reads it as part of the conversation it is already having.
A turn that has no further step - one already answering - cannot take it, and the call says so rather than silently dropping it or holding it for a turn that may never come.

**A step boundary, not sooner.**
A turn in the middle of a provider call cannot be given a message: the request has gone, and the answer coming back was formed without it.
Nothing here interrupts a step, for the same reason §4.4.2 gives about `agent.interrupt`, and `taken_at_step` says which step actually read it so a surface can show the message landing where it landed rather than where it was typed.

**It is durable before it is answered.**
`user/steer` is on the journal whether or not a step ever reads it, carrying `taken` to say which happened.
A message the caller was told was accepted, and which then vanished from the history because the turn ended first, would be the worst outcome available: the person believes they have said something and the transcript disagrees.

**It is not a `user/message`.**
The two derive to the same role, and a reader replaying the journal must still be able to tell a message that opened a turn from one that arrived during it - they answer different questions about how a conversation went, and only one of them can be refused for arriving too late.

**Refusals.**

| Refused | Code | `data` |
| --- | --- | --- |
| the session is not one this server can open | `SessionNotFound` | `{ session_id }` |
| no turn is running | `SessionBusy` | `{ session_id, turn: null }` |
| the running turn will take no further step | `SessionBusy` | `{ session_id, turn }` |
| `content` is empty | `InvalidParams` | `{ field: "content" }` |

The idle case is `SessionBusy` with a null turn rather than a code of its own, and the wording is worth reading twice: a session that is *not* busy is exactly what makes steering impossible, so the code names the condition the caller must fix - there is no turn to join - and the null says which way round it is.
A caller that meant to start a turn calls `agent.prompt`, which is the call that does.

No new error code, and no change to `agent.prompt`.
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

`session.fork` adds none either, for the same reason: §4.4.6 lists each of its refusals against a row above, and a refused fork is either a source that is not there or a parameter that is wrong.

§4.4.7 adds none, and it is the case worth stating twice.
A denial is not a failure of anything: it is the seam working, so it takes no code at all and travels as a `tool/result` the model reads.
What the section does refuse takes rows above - a policy outside the two words is `InvalidParams` naming `policy`, a session that is not there is `SessionNotFound`, and asking with no turn open is `Internal`, because a caller cannot have caused it from the wire.

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

**A reserved call does not bump the minor.**
`Reserved` (§4.2) freezes a shape without serving it, so a peer that knows the call and one that has never heard of it are answered identically: `NotImplemented`, and no capability.
The bump lands with the version that serves the call, which is the first version a capability check can tell apart from the one before it.

**A surface reads `PROTOCOL_VERSION`; it does not spell the version.**
The constant is in `tetanus-protocol`, and a literal `"1.0"` in a consuming lane turns a minor bump into an edit and a failing case in that lane.
The version is the one string in this contract that no peer should hard-code.

**An added field is minor on the wire and not always minor in Rust.**
A JSON reader ignores a field it was not sent; Rust does not, so adding even an optional field can stop the other lane compiling.
Wire-optional is not source-optional.

Two shapes break, not one, and the second was missed when this rule was first written.
A **struct literal** must name every field, so a lane that *constructs* the type stops compiling.
An **exhaustive destructuring pattern** must also name every field, so a lane that only ever *receives* the type stops compiling too.
`crates/cli/src/render/timeline.rs` is the live example: it never builds a `KnownEvent`, and it still cannot survive a field being added to `KnownEvent::ToolResult`, because it matches that variant by naming all four.

So the rule is about both, and a fourth rule joins the three below to make it avoidable: a consumer matches a struct variant with a rest pattern.
Until a type's consumers do, an addition to it is a change both lanes land together, and an addition goes on the types nobody names field by field.
That is why §4.4.6 puts a forked session's lineage on its `session/start` line rather than on `SessionInfo`, and why §4.3's `tool/result.code` is documented but not yet a `KnownEvent` field.

**A major bump covers everything else.**
Removing or renaming a method or field, changing a field's type, making an optional field required, narrowing an accepted value, or changing what an existing error code means is a major bump.

Four rules make additions safe, and a surface must follow all four.

1. Ignore unknown object fields.
2. Ignore unknown notification methods, and answer unknown request methods `MethodNotFound`.
3. Render unknown enum variants through the `Other(String)` fallback, and unknown error codes through their raw code.
4. Match a struct variant with a rest pattern, so a field added to it is not a build break.

The conformance cases in §6 hold the first three to their word.
The fourth cannot be checked from this side - it is a property of the consuming lane's source, not of any value that crosses the boundary - so it is a promise, and the cost of breaking it is paid by whoever adds the next field.

## 6. Verification

The cases run offline.
Those over the boundary types alone live in `crates/protocol/tests/wire.rs`; those that hold the engine's own output to §4.3.1 live in `crates/engine/tests/contract_events.rs`, and those over §4.4.6 in `crates/engine/tests/fork.rs`.
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
| §4.3 a synthesized result carries a code, and an unknown one is readable | TC-PROTO-50 |
| §4.3 the typed path cannot see the code yet, and says so | TC-PROTO-51 |
| §4.3.1 every event the engine writes parses, and all ten types appear | TC-CONTRACT-1 |
| §4.3.1 the engine's own `tool/result` names and cites its call | TC-CONTRACT-2 |
| §4.3.1 `TurnSummary.content` restates the last `assistant/message` | TC-CONTRACT-3 |
| §4.3.1 `assistant/chunk` names the step it belongs to | TC-CONTRACT-4 |
| §4.3.1 a chunk keeps its variant | TC-PROTO-13 |
| §4.3.2 a staged type parses to `None` and keeps every documented key | TC-PROTO-16 |
| §4.3 unmeasured facts are absent, never zero | TC-PROTO-14 |
| §4.3 a withheld value is spelled one way and travels as an ordinary value | TC-PROTO-17 |
| §4.3 the two redaction rules compose by union | TC-PROTO-45 |
| §4.3 the sentinel is not proof, and says so | TC-PROTO-46 |
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
| §4.2 a reserved call answers `NotImplemented`, and is not advertised | TC-ENG-4 |
| §4.2 a reserved method is routed, not unknown | TC-RPC-12 |
| §4.3.1 lineage on `session/start` is optional in both directions | TC-PROTO-19 |
| §4.4.9 every origin fact is optional, and absent means absent | TC-PROTO-30 |
| §4.4.9 a copy and a delegation are told apart | TC-PROTO-31 |
| §4.4.9 depth counts levels and survives a round trip | TC-PROTO-32 |
| §4.4.6 a fork names its source, and the boundary is `through_seq` | TC-PROTO-18 |
| §4.4.7 an ask names its audit line, its tool and its call | TC-PROTO-20 |
| §4.4.7 the four outcomes, and only one of them grants | TC-PROTO-21 |
| §4.4.7 an outcome the engine does not know denies rather than failing to parse | TC-PROTO-22 |
| §4.4.7 the two policies, and a third word that stays readable | TC-PROTO-23 |
| §4.3.2 the three `approval/*` types stage like the other two | TC-PROTO-24 |
| §4.4.6 lineage is on the child's header, and an empty parent forks | TC-PORT-FORK-1 |
| §4.4.6 the child inherits the prefix, and the two journals are detached | TC-PORT-FORK-2, TC-PORT-FORK-3 |
| §4.4.6 an earlier boundary stands while the parent's tail is open | TC-PORT-FORK-4 |
| §4.4.6 a boundary must be a closed one | TC-PORT-FORK-5, TC-PORT-FORK-10 |
| §4.4.6 the child's own work begins after the seed | TC-PORT-FORK-6 |
| §4.4.6 what a fork refuses, and with which code | TC-PORT-FORK-7 .. TC-PORT-FORK-12 |
| §4.4.6 a forked session is an ordinary session | TC-FORK-1, TC-FORK-2 |
| §4.3.2 `context/snapshot` stages, and carries its parts | TC-PROTO-25 |
| §4.4.8 the joining rule reproduces what the model read | TC-PROTO-26 |
| §4.4.8 an empty part contributes nothing | TC-PROTO-27 |
| §4.4.1 a matching major is accepted, and nothing else is | TC-ENG-1, TC-ENG-2 |
| §4.4.2 a prompt runs the documented turn and answers with its summary | TC-AGENT-1 |
| §4.4.2 the pushes a subscriber gets are the journal the turn wrote | TC-AGENT-2 |
| §4.4.2 a turn the output cap cut off ends `max-tokens`, on the call and on the journal | TC-CAP-1 |
| §4.4.2 a step the cap cut off dispatches no tool calls | TC-PORT-CAP-3, TC-CAP-2 |
| §4.4.2 the reason is the turn's, and does not carry into the next | TC-PORT-CAP-2 |
| §4.4.2 the cut-off step's message carries no call | TC-PORT-CAP-4 |
| §4.4.2 a guarded turn names which guard stopped it | TC-PROTO-40 |
| §4.4.2 a guard reason is a value, not a variant | TC-PROTO-41 |
| §4.4.3 a reserved call's capability is not advertised | TC-SUB-5 |
| §4.4.4 which closers a journal needs, and which it does not | TC-PORT-REPAIR-1 .. TC-PORT-REPAIR-10 |
| §4.4.4 `session.create` applies them, and `last_seq` counts them | TC-SESS-6 |
| §4.4.4 a journal is repaired once, not once per open | TC-PORT-RESUME-3 |
| §4.4.5 `from_seq` is a seq and is inclusive, on both calls | TC-PAGE-2, TC-SUB-6 |
| §4.4.5 a `from_seq` past the tail is a catch-up with nothing to catch up on | TC-PAGE-7, TC-SUB-6 |
| §4.4.5 `limit` is clamped down, and zero reads as absent | TC-PAGE-3, TC-PAGE-6 |
| §4.4.5 `next_seq` and `eof` page a journal to its end | TC-PAGE-2, TC-PAGE-4 |
| §4.4.5 `last_seq` is the boundary replay and live delivery join at | TC-SUB-1, TC-SUB-2 |
| §4.4.5 a push reaches only its own session's subscriptions | TC-SUB-7 |
| §4.4.10 a steer names the turn and the step that read it | TC-PROTO-35 |
| §4.4.10 a steer that was never read is still on the journal | TC-PROTO-36 |
| §4.4.10 an idle session refuses with a null turn | TC-PROTO-37 |
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
| 1.0 | States what a turn the provider cut off at its output cap looks like (§4.4.2): `turn/end` carries `stop_reason: "max-tokens"`, `agent.prompt` still answers a summary, the reason is the turn's rather than the cut step's and does not leak into the next turn, and the truncated step dispatches no tool calls, because a completion that stopped mid-write can have stopped mid-arguments. Until now the boundary had no way to say an answer was incomplete: such a turn ended `"natural"`, which reads as a model that had finished. No type changes: `"max-tokens"` is a value of the growable `StopReason` by §7.5, and §7.6's mapping carries it as the fallback exactly as it carries `"interrupted"`. The engine change that ends such a turn lands on its own, with the §6 rows for these clauses; it adds a reason to `tetanus_turn::StopReason`, which by §3 no surface may match - the one surface that does today (`crates/cli/src/main.rs`) needs the arm that carries it, and that arm is the presentation lane's to write. |
| 1.0 | States what the `assistant/message` of a cut-off step carries (§4.4.2): the §4.3.1 fields as always, with `tool_calls` empty and the provider's own finish reason kept. No type changes. The clause above says such a step dispatches nothing; it did not say what the durable message holds, and a call left on it is a call no `tool/result` answers, so the history a client derives asks the provider for a result that will never come and the next request on that session is refused. The raw calls stay on the cited `assistant/chunk` events. The engine change that does this lands with the rest of the cut-off behaviour. |
| 1.0 | States how a journal is read (§4.4.5): `from_seq` is a seq and is inclusive on both `session.events` and `session.subscribe`, a `from_seq` past the tail is a catch-up that had nothing to catch up on rather than a fault, `limit` is a page size clamped down to the server's maximum of 500, `next_seq` and `eof` page a journal to its end, `SessionSubscribeResult.last_seq` is the boundary replay and live delivery join at, and a push reaches only the subscriptions on its own session. No type changes: every field named here already exists, and this says which of several readings the engine is held to. One behaviour change travels with it: `limit: 0` now reads as an absent limit instead of answering an empty page, because that page stalled a pager - `next_seq` did not advance and `eof` stayed false, so a loop that paged until `eof` never ended. No surface passes a `limit` today, so no caller can observe the answer it replaces. TC-PAGE-3 already cited a "server clamps to its own maximum" promise this document had never made; §4.4.5 is that promise. |
| 1.0 | Reserves `session.fork` (§4.2, §4.4.6): a child journal seeded with a prefix of another one's, so a conversation can be taken a second way without losing the first. The child is a copy, its own `session/start` replaces the parent's one line for one line - which is what keeps `seq` equal to the line index and lets the copied `sourceEventSeqs` stand unrewritten - and the header grows `parent_session` and `fork_seq` (§4.3.1), both optional, so every journal written before this still parses. The boundary is `through_seq` and not `from_seq`, because §4.4.5 spends that name on the *first* event a caller receives and this is the last one a child keeps. No error code is added: §4.4.6 tables each refusal against a row of §4.5. No minor bump either, and §5 now says why - a reserved call is answered `NotImplemented` whether a peer knows it or not, so the bump belongs to the version that serves it. Two more §5 rules travel with that one, both learned here: a surface reads `PROTOCOL_VERSION` rather than spelling it, and an added field is minor on the wire but a build break in a lane that constructs the type, which is why lineage is on the journal line and not on `SessionInfo`. The engine slice that serves the call lands next, and takes the `Served` row and the capability with it. |
| 1.0 | Reserves the decision seam (§4.2, §4.4.7): `ui/approve`, a server-to-client request asking whether one tool call may run, and `approval.set`, the call that writes the session's policy. Four outcomes, of which `allowed-once` alone grants and grants only the call it was asked about; two policies, of which `never` settles every ask `rejected` without asking anyone, which is what makes an unattended run neither hang nor depend on a client answering. The seam fails closed on every way of not getting an answer - no capability, an error, a word outside the four, a connection that dropped - and §4.3 now fixes what reading a growable enum's fallback means, because these are the first two whose fallback the engine reads rather than renders. Three durable types join §4.3.2: `approval/asked` and `approval/decided`, one pair per ask with a shared `id`, and `approval/policy`, whose last occurrence is the session's override. No error code is added: a denial is the seam working, so it is a `tool/result` with `ok: false` and not a failure, and §4.4.7's refusals each take a row §4.5 already has. §4.4.4 grows one closer to match - an `approval/asked` a crash caught mid-question is closed `cancelled` on reopen - and that closer is the reason §4.4.7 requires an open turn to ask at all: the turn is the unit repair closes, so a question outside one could never be closed. Which tools ask is deliberately not on `ToolDescriptor` yet, for §5's reason: it is a type the presentation lane constructs. The engine slice that serves the seam lands next, and takes the `Served` rows and the capabilities with it.
| 1.0 | Serves `session.fork` (§4.2, §4.4.6), and advertises the capability that promises it. The engine copies the prefix, writes the child's header over the parent's line 0 and opens the child through the ordinary create path, so a forked session is listed, paged, titled and prompted like any other and its first turn is numbered after the turns it inherited - a fork is a resume of a prefix. `session/start` now carries `parent_session` and `fork_seq` where one applies. TC-ENG-3 grows the row this change serves. TC-ENG-4 and TC-RPC-12 asserted the not-yet answer of that row and now assert it of `approval.set` instead: the two cases belong to whichever calls are reserved at the time, not to the first one that ever was, so serving a call moves them rather than retiring them. `Reserved` and its default trait body stay documented in §4.2 for the call that still needs them. |
| 1.0 | Publishes `context/snapshot` (§4.3.2, §4.4.8): what a turn told the model about the world outside the conversation - the date, the working directory, the branch - recorded once per turn. It is carried as a user message after the retained history rather than in the system prompt, and that placement is the design rather than a detail: a provider caches a prompt by its longest stable prefix, and a sentence saying what time it is would invalidate that prefix on every request of every session. Only the newest snapshot derives to a message; earlier ones stay on the journal, because it records what happened, but do not travel again - yesterday's date is worse than no date. The record carries the parts and §4.4.8 fixes the joining rule, which is the rule §4.3 already gives prompt sections, so the rendering is reproducible and a surface can still show which provider contributed what. No type changes, no new call and no capability: a runtime context is contributed inside the engine and only the record of it crosses this boundary. `type` is a free string by §4.3, so the staged type is not a `KnownEvent` variant and §4.3.2's two-step rule applies as it did for `llm/retry`. The engine slice that writes it lands separately. |
| 1.0 | States what a journal says about where it came from (§4.3.1, §4.4.9): `cwd`, `spawned_by` and `depth` join `parent_session` and `fork_seq` on `session/start`, all optional so every journal written before this parses unchanged. `spawned_by` is deliberately not `parent_session`: a fork is a copy that begins holding another journal's history, a subagent is a different conversation that another one asked for, and a reader that cannot tell them apart cannot answer either question lineage exists for. A session can be both, which rules out one field with a kind beside it. `depth` is durable rather than held in memory because the bound on delegation has to survive a resume - a subagent whose harness restarted must not come back believing it is a root session. `cwd` is where the session was opened and not where it is now, because a tool may change directory and the header is not rewritten. A fork inherits the origin facts it is a copy of and writes its own `parent_session`. Minor on the wire, and safe in Rust here because the presentation lane matches this payload with a rest pattern - stated rather than assumed, since §5's rule is about types the other lane constructs and the next addition must check again. No call is added and nothing appears on `SessionInfo`, for §4.4.6's reason. The engine slice that writes the fields lands separately. |
| 1.0 | Reserves `agent.steer` (§4.2, §4.4.10), the separate call §4.4.2 promised for a follow-up sent while a turn is running. `agent.prompt` still refuses with `SessionBusy`, which is right for a caller that meant to start a turn and wrong for the commonest thing a person does - notice something mid-answer and say so. The message joins the running turn's inbox and is claimed at the next step boundary, never sooner: a turn inside a provider call cannot be given anything, since the request has gone and the answer was formed without it. `user/steer` joins §4.3.2 and is written whether or not a step ever reads it, carrying `taken` to say which - a message the caller was told was accepted and which then vanished from the history is the worst outcome available, because the person believes they have said something and the transcript disagrees. It is deliberately not a `user/message`: both derive to the same role, and a reader must still be able to tell a message that opened a turn from one that arrived during it. No error code is added; an idle session is `SessionBusy` with a null turn, because a session that is not busy is exactly what makes steering impossible and the code should name the condition to fix. The engine slice that serves the call takes the `Served` row and the capability with it. |
| 1.0 | States what a turn a guard stopped looks like (§4.4.2): `turn/end` carries `stop_reason: "timed-out"` or `"repeated"`, and `agent.prompt` still answers a summary rather than an error, because a bound the deployment chose being reached is the bound working. A guard is a bound on the turn rather than on a request - how long the whole turn may take, and how many times the model may do the same thing - which is what distinguishes these from the request deadline the provider seam already has. The two reasons are separate because they need opposite answers: `"timed-out"` usually means a bigger budget or a smaller task, while `"repeated"` means the model was looping and a bigger budget makes it strictly worse, so collapsing them would leave a reader unable to tell "this needs longer" from "longer will not help". A guard stops at a step boundary like `agent.interrupt`, so the journal is balanced and §4.6 holds unchanged. No type changes and no error code: both are values of the growable `StopReason` by §7.5, carried across by §7.6's mapping exactly as `"interrupted"` and `"max-tokens"` already are. The engine slice that runs the guards lands separately, and it will add reasons to `tetanus_turn::StopReason`, which by §3 no surface may match - the arm belongs to the presentation lane. |
| 1.0 | Settles how `config.dump` decides a value is a secret (§4.3): the schema where the engine has one, the name rule for everything else, and the two compose by **union, never by override**. That direction is the whole change. A schema that could un-mark a key would make adding a key to the schema a way to start publishing it, and the mistake would be silent and permanent. Each rule alone fails one way - a schema misses what it does not describe, a name rule misses a credential called `authorization` - and the union fails safe, at the cost of occasionally withholding a `monkey_token` the user can still read in their own file. The schema half is what catches a credential whose name says nothing, which no rule reading the name could find. One ambiguity in the previous wording is now stated rather than left implicit: nothing distinguishes a withheld value from a document that literally contains `<redacted>`, so a surface must not read the sentinel as proof of secrecy. The honest signal is a flag on the entry, and it is deliberately deferred - `ConfigEntry` is a type the presentation lane constructs, so by §5 that field is a change both lanes land together. No type changes here, and the engine slice that consults a schema lands separately; until it does, the name rule is what runs and its behaviour is unchanged. |
| 1.0 | Corrects two things this document had wrong about itself. §4.3.1's payload table omitted `tool/result.code`, which §4.4.4 has promised since crash repair landed and the engine has written ever since - so the table was not a description of what the engine writes. The row now lists it, and §4.3 says when it is present: only on a result nobody ran, because a call that was dispatched reports its outcome in `ok` and `content` and needs no reason for not having one. And §5's rule about added fields was stated too narrowly. It said a field breaks the lane that *constructs* a type; an exhaustive destructuring pattern breaks the same way in a lane that only *receives* one, and `crates/cli/src/render/timeline.rs` is the live example - it never builds a `KnownEvent` and still could not survive a field on `ToolResult`. A fourth compatibility rule follows: a consumer matches a struct variant with a rest pattern. It cannot be verified from this side, because it is a property of the other lane's source rather than of anything crossing the boundary, so it is a promise whose cost is paid by whoever adds the next field. `KnownEvent::ToolResult` is therefore *not* given the field here: the gap is named instead, with what unblocks it. Today a surface on the typed path cannot tell `TOOL_NOT_STARTED` from `TOOL_OUTCOME_UNKNOWN` - the distinction §4.4.4 calls load-bearing, since one is safe to retry and the other is not - though the value is on `SessionEvent.data` for a reader willing to look. No type changes. |
