# Architecture

## 1. Identification

- **System:** tetanus, the whole Cargo workspace.
- **Version:** 0.1.0, Phase ①, tracking upstream deepseek-harness `0.1.0-rc.7`.
- **Status:** implemented, and covered by an offline suite that is the merge gate.
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
  -> crates/toolset  tetanus-toolset   the one assembly of the tools this build offers
       -> crates/fs, crates/exec, crates/features, crates/mcp, crates/web
  -> crates/turn     tetanus-turn      turn engine, events, LLM seam, tools, boot, trace
       -> crates/session  tetanus-session   durable event vocabulary, JSONL and SQLite journals, projections
       -> crates/core     tetanus-core      registry, services, event bus, effects
  -> crates/config   tetanus-config    layered config with provenance
  -> crates/fs       tetanus-fs        filesystem service, its two backends, file tools, presets
  -> crates/features tetanus-features  the built-in feature tools over the session journal
  -> crates/engine   tetanus-engine    the `Engine` implementation behind the contract
  -> crates/exec     tetanus-exec      subprocess and piped seams, shell backends, persistent shells
                                       and terminals, the shell and terminal tools
       -> crates/sandbox  tetanus-sandbox   the sandbox policy and the Landlock boundary
  -> crates/rpc      tetanus-rpc       JSON-RPC codec and carriers, hosted by `tetanus serve`
  -> crates/ui       tetanus-ui        colour policy, theme, width, redrawable block, scrollable page,
                                       full-screen view loop

crates/mcp        tetanus-mcp        an MCP server on stdio: client, tool bridge, reconnect supervisor
crates/web        tetanus-web        web_fetch and web_search over one transport seam

crates/protocol   tetanus-protocol   the engine/presentation contract (§4.8)
```

`tetanus-toolset` is the one place that says which tools this build offers. Every tool crate is a
named *source* in `sources()` - one line each - and the binary composes that set for both the
catalogue `tetanus tools` prints and the registry each session's turns dispatch from, so the two
cannot disagree. A duplicate name across two sources is refused at composition naming both crates,
rather than resolved by registration order, because the model would otherwise be offered one tool's
schema and run another's body. A deployment selects by source (`tools.sources`), since a crate is
what lands and what a user recognises.

It is the *binary's* assembly and not the engine's on purpose. `tetanus-engine` keeps an offline
minimum - the `builtin` source's tools - because it has no session to key the file tools'
observations on or to fold the feature tools over, and because depending on the assembly would give
it a dependency on every tool crate, which is the line the paragraph below draws. A case holds the
engine's default against `builtin`, so neither may grow a private expression of the other's set.

`tetanus-mcp` and `tetanus-web` are the two crates that leave the machine, and both are composed
into a harness rather than depended on by one: each contributes `tetanus_turn::tools::Tool`
implementations and reads its own section of the settings document. Both are declared as sources
and contribute nothing until the document names them. Everything either of them
decides sits above a seam - a `Link` for a server on a pipe, an `HttpTransport` for a request - so
the whole policy is asserted with no socket in the suite, and a failure out there is a failed tool
call carrying a class rather than a turn that ended.

`tetanus-core` depends on nothing in the workspace.
`tetanus-config` depends on no other workspace crate; the CLI and the engine both read it.
It holds one document per layer rather than one folded map, because a layer that is re-read can
*drop* a key and the value under it has to come back; a folded map has nothing to come back to
([crates/config/src/lib.rs](crates/config/src/lib.rs)).
`tetanus-fs` depends on `tetanus-turn` and nothing depends on it: it is a *consumer* of the tool seam,
not a layer under it, which is what keeps a harness composed without file tools a harness that still
builds and runs.
It reuses the containment walk `tetanus-turn` already carries rather than growing a second one
([crates/fs/src/local.rs](crates/fs/src/local.rs)), so the fenced and unfenced backends differ only in
which root a path is judged against.
`tetanus-features` depends on `tetanus-turn` and nothing depends on it, so a harness composed
without the feature tools still builds and runs.
Every module in it has one shape - a durable event type, a fold over the log, a tool that appends and
answers - because state a replay cannot reproduce is state the harness would lose, and a cache beside
the journal is a second copy that can disagree with it
([crates/features/src/lib.rs](crates/features/src/lib.rs)).
A surface reads that state through one module - [crates/features/src/view.rs](crates/features/src/view.rs) -
rather than through the folds themselves, so the vocabulary a panel is written against is a published
shape with stable field names instead of this crate's own types.
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
| `EventBus` / `Event` | [crates/core/src/events.rs](crates/core/src/events.rs) | An event declares its dispatch mode as a `const`. Registering or dispatching through another mode panics rather than silently doing nothing. An `emit` or `parallel` observer that panics is contained and logged: its peers still run, and the dispatch still returns. A `serial` or `waterfall` listener that panics stays loud. |
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
  -> agent/request -> llm/stream -> [agent/request-error -> llm/stream]* -> assistant/chunk*
  -> assistant/message
  -> tool/call* -> tools/pre-execute -> tools/execute -> tools/post-execute -> tool/result*
  -> step/end -> ...loop... -> agent/turn-stopping -> turn/end
```

The canonical sequence, each event's dispatch mode and output type, and the rationale for the order
are in [docs/turn-flow.md](docs/turn-flow.md).
The live extension points are declared in [crates/turn/src/events.rs](crates/turn/src/events.rs).

One step's `tool/call*` may overlap.
A call whose tool declares itself parallel-safe joins a pool bounded by
`TurnConfig::max_parallel_tool_calls` (default 10); a call whose tool declares itself exclusive runs
alone, after the pool ahead of it drains and before any call behind it starts.
Dispatch may overlap but commitment may not: a `tool/result` is appended only once every earlier
call of the step has been appended, so the journal reads in model order however the calls settled.

A failed call is not a failed turn.
Whatever a tool returns as an error, a call naming no registered tool, and a panic inside a tool body
all become one `tool/result` with `ok: false`, which the next request carries to the model.
A tool body is somebody else's code, and the loop is not its author's to take down.

The engine resolves four services from the registry and names no implementation:

| Service | Key | Provider trait | Phase ① implementations |
| --- | --- | --- | --- |
| `LlmService` | `llm` | `dyn LlmAdapter` | `MockAdapter`, `DeepSeekAdapter` |
| `ToolsService` | `tools` | `ToolRegistry` | `EchoTool` |
| `SessionService` | `sessions` | `dyn SessionLog` | `JsonlSessionLog` |
| `PromptService` | `system-prompt` | `PromptRegistry` | the engine's own base section |

`boot()` ([crates/turn/src/boot.rs](crates/turn/src/boot.rs)) mounts the four providers plus
`AgentLoopPlugin`, which provides nothing and declares the other four as dependencies, so a missing
provider fails at boot naming `agent-loop` rather than mid-turn.

`PromptRegistry` ([crates/turn/src/prompt.rs](crates/turn/src/prompt.rs)) is what one assembly
starts from. A section has a unique name, an explicit order, and text that is either fixed or asked
for at each assembly; registration returns an effect handle, so a plugin's prompt text dies with the
plugin. The engine fills one reserved slot, `base`, from `TurnConfig::base_prompt`, and the
`system-prompt/assemble` waterfall still has the last word over what the registry produced.
The one exception is a section registered as the whole prompt (`Section::complete`): the assembly
still runs in full, so tool schemas and every other contribution still resolve and every listener
still sees them, but the engine restores that section afterwards as the sole prompt section. A
registry holds one at a time, so the second registration is refused rather than shadowing the
first.

The registry also holds the prompt variables a section's text names between braces. They ride the
assembly as `SystemPrompt::variables`, so a listener may add a name or replace a value, and
`SystemPrompt::render` substitutes them as the last thing that happens to section text. Strictly: a
reference this assembly cannot fill fails the turn (`TurnError::Prompt`) rather than reaching the
model as prose it would have read as an instruction.

### 4.5 Information view - the session log

`SessionEvent` ([crates/session/src/lib.rs](crates/session/src/lib.rs)) is the durable record: a
`type`, a `seq` equal to the log length at append time, a `time` in epoch milliseconds, a JSON `data`
payload, and `sourceEventSeqs` on the surface events that cite their inputs.

`JsonlSessionLog` writes one JSON line per event, fsyncs it, mirrors it in memory, then emits
`session/event` on the bus, so observers never poll the file.
`replay()` reads a journal back and verifies `seq` contiguity: a gap means the file is not a faithful
copy of the log that produced it.

`SessionLog` is a seam and not a description of that one writer.
`SqliteSessionStore` ([crates/session/src/sqlite.rs](crates/session/src/sqlite.rs)) keeps every
session of a deployment in one database behind the same trait, and `sessions.backend` picks between
them at boot.
Everything the JSONL journal promises is promised there, including per-append durability: each
append is its own commit under `synchronous = FULL`, because a backend a caller cannot tell apart
must not quietly promise less.
`import_jsonl` and `export_jsonl` move a session between the two, losslessly in both directions -
both writers serialize the same `SessionEvent`, so the round trip is byte-identical.

Model history is *derived* from the log by `derive_messages`
([crates/turn/src/log.rs](crates/turn/src/log.rs)), never stored beside it.
Model-visible means logged.
The converse is not true, and the `approval/*` events are the case that shows it: they are durable
and replayable and derive to nothing, because what the model learns about a denial is the
`tool/result` it gets, not the audit of how that was decided.

That audit is the decision seam ([crates/turn/src/approval.rs](crates/turn/src/approval.rs)), which
decides whether one tool call may run.
It is worth reading for one structural reason: the session's policy is a *fold over its own journal*
rather than a field anywhere, so a resumed session is under the policy it was under with nothing to
replay but the log.
The seam fails closed - a grant is one specific word from an answerer that ran and returned, and
every other path denies - and the `never` policy is applied by `request` itself rather than by a
listener, because a listener registered later could be ordered ahead of a gate listener and answer
first.
It is also the one place `waterfall` is contained rather than loud: a question that cannot be
answered has a defined answer, so a panicking answerer denies its call instead of failing the turn.

The turn engine applies that seam between `tools/pre-execute` and the dispatch: a tool says whether
one pending call needs deciding, and a call nobody granted is never dispatched at all - the step gets
a `tool/result` with `ok: false` and a `code`, which is what §4.4.7 of the contract means by a denial
being a result rather than a failure.
After `tools/pre-execute`, deliberately, so that what is decided is what would actually run.
Asking the user something ([crates/turn/src/questions.rs](crates/turn/src/questions.rs)) is the same
shape for a different question, down to the durable pair and the enclosure rule, and differs in what
counts as an answer: every question covered, every selection a label that was offered.
Both are unbounded waits with exactly one way out, which is why an interrupt withdraws an outstanding
question rather than only stopping the turn at its next step boundary.
Raw `assistant/chunk` events stay on the log so a UI can replay a stream exactly as it arrived, while
the `assistant/message` that cites them is what enters history.

Derivation is also where compaction happens
([crates/turn/src/compaction.rs](crates/turn/src/compaction.rs)), and that is the whole design.
A journal is append-only, so a conversation that has outgrown its context window cannot have its
older span deleted or rewritten.
Instead a `compaction/summary` record names the events it shadows and the surface event immediately
after it stands in their place; `compaction::surface` applies that rule wherever history is derived.
A replay therefore reproduces the compacted history from the same records rather than from a second
stored copy that could disagree with them.
The adjacency of a record and its replacement is contractual rather than tidy: it is what lets a
consumer with bounded state price a replacement without keeping a price per message.
A cut never separates a tool call from its result, judged over the current surface rather than over
step markers, because compaction moves surface positions and step markers do not follow.

The folds a reader wants over that log are projections
([crates/session/src/projection.rs](crates/session/src/projection.rs)): a named `init`/`apply`/`view`
per domain, driven forward as events commit.
A unit contributes mathematics and nothing else - no clock, no subscription, JSON state - which is
what lets a value be checkpointed at all, since anything a projection knows could have been
recomputed rather than remembered.
The two that price nothing are [crates/session/src/units.rs](crates/session/src/units.rs), so a
listing that wants a title need not link a provider adapter; the three that do are
[crates/turn/src/projections.rs](crates/turn/src/projections.rs), beside the estimator they share.
Each step writes a `request/context` record before it dispatches, carrying the route, its window and
what the system prompt and tool catalog cost, which is what the context breakdown anchors on and
what lets a turn a provider failure ended still say what it tried to send.

What a run *works out* rather than what happened to it goes in the key-value store
([crates/core/src/storage.rs](crates/core/src/storage.rs)): declared tables of JSON in one file,
replaced whole by an atomic rename.
A projection checkpoint, a computed title and a cache each belong there - reproducible from the log,
expensive enough to keep, and not facts, so not journal entries.
A payload too large to carry goes to the spill store
([crates/core/src/spill.rs](crates/core/src/spill.rs)), which keeps the whole thing in an owner-only
file and hands the model a bounded preview and a locator. It has two doors, and which one a caller
uses follows from who holds the bytes: a finished payload is `save`d, while a producer that is
*dropping* bytes as it goes - `crates/exec` bounding a command's output - `open`s a writer and
streams into it, because by the time a result exists what it dropped is gone.

A credential goes in neither, and in particular not in the settings document
([crates/config/src/credentials.rs](crates/config/src/credentials.rs)).
The document is read into layers, published by `config.dump`, quoted in diagnostics and pasted into
bug reports; `crates/config/src/secret.rs` exists to redact values that should never have been there.
The credential store is the process environment over an owner-only file, and its values never enter
a layer at all, so there is nothing for a surface to forget to redact.
The environment wins and is visibly read-only: a key supplied at launch is the run's explicit intent
and nothing in the process can edit it, so a write against a reference it supplies is refused rather
than accepted into a file that resolution would then ignore.

The other durable record is the settings document: `settings.yaml` (or `.json`) under the harness
home, which is `$TETANUS_HOME` or `~/.tetanus`
([crates/config/src/file.rs](crates/config/src/file.rs)).
It holds sections, and resolution is flat, so a section reads as its leaves under a dotted key -
`log: {level: debug}` resolves `log.level`.
An absent document is a first run, not a fault; every other fault is loud, because a document the
harness silently ignored would leave the user configured on paper and unconfigured in fact.
The engine turns that document into the settings it runs on
([crates/engine/src/boot.rs](crates/engine/src/boot.rs)), over its own compiled defaults, so the keys
`config.dump` reports and the keys a document may set are one list rather than two.
A value of the wrong type fails the boot for the same reason: a refused setting is visible, and an
ignored one is not.
The retry policy is resolved there too ([crates/engine/src/retry.rs](crates/engine/src/retry.rs)),
under the `llm.retry` keys upstream uses, because the decision and the executor
([crates/turn/src/llm/retry.rs](crates/turn/src/llm/retry.rs)) read no settings themselves.
Which binary calls it, and with which flags, is the presentation lane's wiring.

`tetanus config` is the first of that wiring, and it lives in [crates/cli/src/settings.rs](crates/cli/src/settings.rs) rather than in `main.rs`:
`main.rs` is the binary's hub, and a command that keeps its body there widens the hub for every other command.
It reads the document through `boot::document`, settles it through `EngineConfig::from_settings`, and prints the provenance the engine resolved rather than a list of its own -
so the keys the page shows and the keys a document may set are one list, and a key the engine gains appears without this crate being told.
Both steps can fail and neither failure is stepped over.
A document that cannot be read is `Io`, exit 1, and names the path once, because the engine's own sentence already names it.
A value a key does not take is `InvalidParams`, exit 2, and names the field; its next step names the document rather than `--help`, since nothing in a document is a flag.

Every other subcommand that builds an engine boots the same way, through one reader in `crates/cli/src/main.rs`.
A subcommand that resolved settings of its own would make `tetanus config` describe a harness the next command does not run.
Which document that reader opens is settled once for the whole process, before any subcommand starts: `--settings <path>` names it, and without that flag it is `settings.yaml` under the harness home.
The flag is global, so the same path governs whichever subcommand it was typed at and on whichever side of it, and one answer for the process is what keeps `tetanus config` describing the document the next command will read.
A document nobody named may be absent, because a first run has none and the answer is then the compiled defaults; a document the user named may not, and a path with nothing there is `Io`, exit 1, reported before the harness has quietly started on those defaults.
The flags the user typed go on top as the `Flag` layer, which outranks `File`: `--dir` sets `sessions.root`, so it still wins over a document and `config.dump` reports the key as `flag` rather than as `file`.
A flag that was not passed sets nothing at all, which is what leaves the document able to win - a clap `default_value` on that layer would be a document that could never win, so the flags that override a setting are optional and their defaults are the engine's.
Whoever set a refused value is who the report is for: a value off a flag reads as any other bad argument, and a value off the document names the document.
The page itself is the engine's own `config.dump` rather than a copy of the resolved layers, so a key whose name says it holds a credential keeps its row and its layer and loses its value, as section 4.3 of the interface contract requires.
`tetanus config --dir <path>` asks the question rather than giving an instruction: it lists nothing and opens nothing, and it is how the `flag` layer can be read at all, since a flag is only on the layer of the process it was typed at.
`tetanus config --defaults` asks the other question a reader has here: not what is set, but what this build settles when nothing is.
It reads no document at all, so it answers about the build rather than about the machine, and it still answers when the document that would have covered it cannot be read - which is when the question is most often asked.
A page that is not what the harness will run on has to say so, and it says it on stderr, so the bytes a script reads are the bytes the other page gives it.
`--dir` with it is a usage error: a flag that overrides a setting and a page that reads no settings are two questions, and answering one while being asked both would print a `flag` row on a page whose whole claim is that nothing was set.

The two subcommands that run turns, `run` and `chat`, take four settings that way: the provider, the model, the step budget, and the root the journal is written under.
Each has a command default that is not the engine's - `run` falls back to the mock adapter because a first run must need no credential, and `chat` falls back to DeepSeek because a conversation with the mock is a demonstration rather than a use.
So the compiled default of a key cannot decide between them, and what is read instead is the layer: a key still on the `Default` layer is a key nobody has an opinion about and the command's own fallback stands, while anything above it - a document, an environment, a flag - is somebody's opinion and outranks a default this binary happens to compile in.
A model nobody set stays unset rather than becoming the engine's compiled one, because that model belongs to the engine's compiled provider and offering it to an adapter that never advertised it would name a model that does not exist; an unset model is the adapter's first catalogue entry.
A provider name no adapter in this build answers to is refused where the document is named, since clap refuses an unknown `--adapter` and a name that got this far came out of a document or an environment.

The three events that derive a message - `user/message`, `assistant/message`, `tool/result` - are the
*surface*: the part of the log the model sees.
[crates/turn/src/tokens.rs](crates/turn/src/tokens.rs) prices that surface, and any request, under
one fixed heuristic, so a context gauge and a compaction decision read the same number.
It is deliberately crude, and it is the only estimate available until a provider reports usage.

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
  Every turn runs that shape, not only the first: it reads this step's own messages, never the whole
  conversation. Which tool it calls is the prompt's to choose: a prompt opening with `!` asks for
  `shell` with the rest as the command, so a build with no key still exercises the process seam.
- [crates/turn/src/llm/deepseek.rs](crates/turn/src/llm/deepseek.rs) - DeepSeek chat completions
  behind an `SseTransport` seam, so the request body and the stream decoder are tested without
  network. Credentials are referenced by environment variable name; config never carries a literal
  key.

Only the `[DONE]` sentinel finishes a stream.
One that stops before it fails as `PROTOCOL` instead of returning what arrived, because half a message
read as a whole answer is a wrong answer rather than a short one, and nothing after the sentinel is
decoded.

Every failure carries a stable code (`LlmError::code`), and
[crates/turn/src/llm/retry.rs](crates/turn/src/llm/retry.rs) decides from that code whether another
attempt is worth making and how long to wait first.
A refusal also carries the provider's own id for the request it refused, read off the response
headers (`llm::REQUEST_ID_HEADERS`), because that id exists only in the provider's logs and is the
one fact about a failure this harness cannot reconstruct from what arrived.
It travels three ways: to a recovery listener on `RequestFailure`, onto the `llm/retry` record when a
policy recovered and nobody was told, and into the published error's `data` when the failure reached
the caller ([docs/interface-contract.md](docs/interface-contract.md) section 4.5).
The policy is a value that decides, not a loop that waits: it returns the delay instead of sleeping,
which is what keeps its cases offline and free of a clock.

A failed call is offered to `agent/request-error` before it ends the turn, and a listener there may
ask for the same request to be sent again.
That is the seam `retry::install` occupies, and it is why the driver needs no clock of its own: the
waiting belongs to whoever asked for the retry.
The listener is scoped to one provider, because a policy belongs to a provider rather than to the
engine, and a failure from another route is delegated on untouched.
Each scheduled retry is durable before its wait - `llm/retry`, then `llm/retry-started` when the wait
is over ([docs/interface-contract.md](docs/interface-contract.md) section 4.3.2) - so a surface says
"retrying" instead of showing a stalled turn, and so the attempt count is read back from the journal
rather than held in the listener.
Nothing resolves a policy out of settings yet, so a composer passes one in; that half is phase ②
([docs/parity.md](docs/parity.md) section 4).

### 4.7 Interface view - surfaces

The main surface is the `tetanus` binary ([crates/cli/src/main.rs](crates/cli/src/main.rs)).
It carries the nine subcommands [docs/interface-contract.md](docs/interface-contract.md) §4.7
defines - `run`, `chat`, `sessions`, `replay`, `models`, `tools`, `config`, `serve`, `info` - each
identified there by the contract calls it makes rather than by what it prints. See
[README.md](README.md#cli).

`tetanus run` shows one turn three ways, and every settled line in all three comes from the same
`Reader` in [crates/cli/src/render/timeline.rs](crates/cli/src/render/timeline.rs), so a turn watched
live reads like the same turn replayed tomorrow.
The default is a block under the shell prompt, redrawn in place by `Screen`, which holds it to the
terminal it is drawn on: the last rows of what it was handed, one row kept for the prompt the
cursor sits on.
A block as tall as the terminal scrolls its own top away, and the cursor arithmetic that redraws
the next frame then counts from the wrong row - the view duplicates itself down the screen and does
not stop until the run does.
The caller keeps the block short for its own reasons; "short" is a number chosen against a terminal
it cannot see, and this type can see it.
That block says what the turn is waiting on, how long it has been waiting, and what it has spent so
far ([crates/cli/src/render/live.rs](crates/cli/src/render/live.rs)): a reader watching a turn is
watching it spend money, and a figure that arrives only once the turn is over tells them
afterwards.
The count moves a step at a time, because a step is billed when its message settles, and it is the
same running total - and the same wording - the closing line reports; a build whose messages carry
no usage says nothing rather than saying nothing was spent.
The browser panel draws the same figure on its turn card, from the same events.

A failed turn ends the turn, not the chat.
The engine is still booted and the journal is still open, so the fault is settled onto the page
under the question that drew it and the next line is asked for: a provider that could not be
reached is the ordinary failure here, and dropping a reader back to a shell to retype
`tetanus chat -s <path>` answers a network blip by throwing away the conversation.
Piped input keeps the other behaviour, because there is nobody there to ask again - the status
§4.5 gives the code is what a script reads, and a run whose turns all failed must not exit 0.

Only one way of ending is painted as a turn that ended well.
A model that stopped because it had finished is `natural`; every other reason means the answer on
the page is missing something a reader cannot see is missing - the provider's cap cut it off, a
step budget ran out, a listener refused the step, somebody interrupted - so the reason is drawn in
the warning colour rather than the colour of a job done.
The cap is worded rather than echoed (`cut off at the output cap`, not the wire's `max-tokens`) and
carries a sentence of its own, because the contract asks for exactly that in §4.4.2: a surface that
renders it as an ordinary end tells the reader that a sentence the model never finished is the
whole reply.

A turn's closing line adds how fast the model was, on the turns slow enough for that to be a fact:
the wait for the first token, and the rate the answer decoded at
([timeline.rs](crates/cli/src/render/timeline.rs)).
Upstream's own turn footer carries the same pair and folds it the same way - the first step's wait,
because a later step is waiting on a tool rather than on the model, and a rate over the steps that
recorded both halves of it.
Both are derived from event times the journal already holds, so a surface cannot disagree with the
journal it read, and both sit behind the threshold the duration sits behind: under a second they
are noise, and two runs of one turn must print the same bytes.

`/stats` on the full-screen chat is the same arithmetic over the whole journal rather than one turn:
how much was asked, how long the model and the tools each took, the average wait for a first token,
the rate, and what was billed
([`stats`](crates/cli/src/render/timeline.rs)).
Upstream keeps those figures on a strip beside its composer; a terminal has no room for a strip that
keeps itself up to date, so this is a snapshot that stays where it was asked for - and a reader who
wants the figure again asks again, which is cheaper to read than a number moving in a corner.
A group with nothing in it is left out whole: `0 tokens` reads as a conversation that was free,
where a conversation whose every request failed is one that never got an answer.
`--ui` takes the whole terminal instead and composes each frame with `Page`
([crates/ui/src/page.rs](crates/ui/src/page.rs)), which is what makes a turn scrollable while it is
still running.
`--json` prints the contract's own result types and draws nothing.

A `--ui` is refused where there is no screen to hold it, at the moment the flag is read: a
redirected stdout, and a terminal whose `TERM` says it cannot address a cursor - `dumb`, or unset.
Both are §4.5's exit 2, because a flag the terminal cannot honour is a bad argument rather than a
failed command, and the two are worded apart: a pipe is something the reader undoes, `TERM` is
something they set, and telling somebody sitting at a terminal that this needs one reads as a bug
in the binary.
All three read their events from the session log the engine is writing rather than from the bus: the
journal is the durable record, and polling it is what keeps the presentation lane out of the engine.
Which is also why the `--ui` view has to be told when the turn is over: a turn that fails writes no
closing event, so the only record of the failure is the value the future returned, and a view left
polling a journal that stopped growing would keep saying the turn was running.
The reason is settled onto the page in the wording [crates/cli/src/render/fault.rs](crates/cli/src/render/fault.rs)
gives every other surface, because stderr is behind the alternate screen until the reader gives up.
Being told is also what lets the view go quiet: from there the frame it composes every 80ms is the
frame already on the terminal, and `Frame` is comparable so that the paint is skipped rather than
sent again.
The two views driven by `tetanus_ui::show` never need this - a page over something finished waits an
hour for a keystroke - and this one cannot wait, because the turn is still arriving.

Whichever of those a run was asked for, stderr is told the same thing by the same rule
(`status_line` in [crates/cli/src/main.rs](crates/cli/src/main.rs)).
The four ways of running a turn once held three answers between them and one of them held none, so
`tetanus run --json > events 2> log` left `log` empty while dropping `--json` filled it.
The rule asks one question - does this view already show the turn on stdout - and writes the status
unless that would put a second spinner on a screen that already has one.
A stderr that is not a terminal never can, so it always gets the line, degraded to one plain
sentence: which human view stdout was asked for is not a reason to write a different log.

Whatever the view, the wait itself is one line on stderr
([crates/ui/src/progress.rs](crates/ui/src/progress.rs)): the phase, a spinner while the stream is a
terminal, and - once the phase has run longer than two seconds - how long it has been running.
A spinner says the process is alive and not whether this call has been going for four seconds or
four minutes, which is the question a reader waiting on a model actually has; a slow answer and one
that will never arrive look identical until something counts.
The plain form a redirected stream gets carries no clock: a log wants one line per phase, and a
duration in it would make two runs of one turn print different bytes.

`tetanus chat --ui` ([crates/cli/src/render/fire.rs](crates/cli/src/render/fire.rs)) is the whole
conversation as one screen: the transcript above, kept by the view because the alternate screen has
no scrollback to keep it, the turn arriving in the block under it, and one row at the foot that is
always the row you type on.
The editor answers a key before the view does, which is what makes it a place a person can type:
every printable key belongs to the line, `?` and `q` included, and only the keys `Line` does not
answer - the arrows, the page keys, Escape - move the window.
It is the one full-screen view with a cursor, and `Frame::cursor` exists for it: the prompt row is
the one place on a screen where the terminal's own caret says something true, and a drawn stand-in
blinks at the repaint's rate, hides the character under it and is invisible to a screen reader.
The commands are the chat's own, so a reader who knows `/help` and `/exit` needs no second
vocabulary; the keys, the statuses and the wording are the ordinary chat's too, which is what lets
a script wrap either.
The view keeps what was said rather than the rows it drew - an event, a card, a note, a fault - and
composes the rows again whenever the width changes, so a terminal that is narrowed folds the
conversation into it rather than cutting each row's tail off, and one that is widened puts the
folds back.
`Page` does not rewrap, and says why: a live view must not rewrite history under a reader who is
still reading it. A resize is not the stream rewriting anything, it is the reader asking for a new
shape at the moment they ask - the same reading [`browse`](crates/cli/src/render/browse.rs) makes
of the same rule.

What each key does is `/keys`, and looking through what was said is `/find word`.
Both are commands rather than keys for the same reason `?` is not the key card here: `/` is a character in the line being typed, and a view that
took it is one where a reader cannot ask about a path.
The matches are marked where they are drawn, walked with ctrl-n and ctrl-p - two of the keys the
editor does not answer - and counted on the footer, which is also where a word no line holds is
said in words rather than as `0 of 0`.
They are kept as line numbers and found again after a rewrap, because a resize moves every line and
a reader who widened their terminal did not ask to lose their search.

The up and down keys walk what this reader has asked, because that is what they do at every other
prompt a person has used and a chat is a prompt; the page keys scroll, which is what scrolls a page
everywhere else.
A half-written line is kept when the walk starts and comes back at the end of it - a reader who
pressed up to check what they asked last time has not thrown away the question they were writing -
and a question asked twice in a row is kept once, because pressing up means the question before
this one.

Every view that polls a journal asks the filesystem whether it has grown before it asks the log for
its events ([`appended`](crates/cli/src/main.rs)).
A journal is append-only, so a file the same length as last time holds nothing this view has not
seen; the rule is that the length *changed*, not that it grew, so a journal truncated or replaced
under the view is read again rather than trusted - and `SessionLog::events` copies every event it holds, which a view polling twelve times a
second turns into a cost that grows with the conversation.
Measured on a journal of six thousand events: a sixth of a core, spent on a conversation nobody was
having; one `stat` a frame instead.
`run --ui` had the same burn for as long as its view was left up - the view outlives the turn, and
the poll outlives both - and the plain block and the `--json` stream poll through the same helper
now.
What a second process appends is not on this page either way: those lines are on the file and not
in this log's memory, and reading a file back is what `tetanus replay` is for.

A conversation with nothing asked in it opens on a page that says so, names the journal every turn
will be appended to, and points at the two commands.
A blank screen with a prompt on it is a screen that might be broken; the browser panel says
`Nothing said yet. Ask something below.` for the same reason, and a terminal has the room to say
the rest of what a reader needs.
It is one of the things that were said rather than a case in the frame, so it rewraps with
everything else - and the first turn takes it away rather than leaving it further up the transcript,
because what it says stops being true the moment one exists.

The prompt grows for a question longer than a row, up to five of them, and the transcript gives up
those rows: what is being written is what the reader is looking at, and a one-row prompt that
scrolls sideways is one they cannot check the sentence in.
It is broken at the column rather than between words, because these rows are a text box and not a
paragraph - a word break moves every character after it, so the caret a reader is steering would
jump a row as they typed a space.
Past five rows it scrolls inside them, keeping the rows the caret is on, the way the single row
scrolled sideways.

The caret is placed from where the prompt began rather than from the bottom of the screen, and only
when that row was drawn: a terminal too short for the whole arrangement drops what does not fit -
the footer first - so a caret counted from the footer lands on the rule above the prompt, and a
terminal with no room for a prompt at all has nowhere to put one.
A terminal no columns wide draws nothing and is pointed at with nothing; terminals report that
width while they are being resized, and a view that fell over there would take the conversation
with it.

Every arrangement in the binary is swept at those sizes now, and each has a case of its own:
[`Page`](crates/ui/src/page.rs), which every view over a stream is built from, and the key card
([`keys`](crates/cli/src/render/keys.rs)), which spends four rows on furniture and counts the keys
it could not show.
The other two full-screen views hold as well:
the journal ([`browse`](crates/cli/src/render/browse.rs)), which refills on every width change, and
the picker ([`pick`](crates/cli/src/render/pick.rs)), which composes its own frame so that the
window can follow the cursor.
Neither costs anything while it is up: they wait on a key rather than polling a journal, which is
what a view over something already written can do.

`/think` and `/more` open what is already on the page: the model's thinking, folded to its first
line, and a tool's result, capped at sixteen lines so one long result cannot push the answer it led
to off the screen.
Both are toggles and both compose the conversation again, which is what makes them a reader
changing their mind rather than a flag they had to know about before they started - `--think` is
the flag, and a screen that can rewrite what it drew does not need one.
The browser panel opens the same card by clicking it; a terminal has a command instead, and the
ordinary chat, whose page is the reader's own scrollback, answers both by naming the flag.

The keys card lands on the conversation rather than over it, which is the other difference from
every full-screen view that opens one with `?`: a reader who asks what a key does while reading
something should still be reading it afterwards.
It names the editing keys as well, because those are the ones nothing else on the screen says - the
footer says Enter asks, and nothing says alt-b walks back a word.

Tab and Shift-Tab walk the turns, which is this terminal's answer to the message list the web panel
puts beside a conversation: there is no list to put there and no pointer to click it with, and what
a reader reaches for is the start of a turn three or thirty back.
A turn's opening line goes to the top of the body, because what follows it is the turn.
Walking past the last turn lands on the foot of the conversation and walking before the first lands
on its top: a reader going one way or the other is heading for that end, and neither is a refusal
to move.
The turn's own line numbers are kept beside the search's, and found again for the same reason.

One row of the screen says nothing: the rule between the conversation and the row being typed on.
What a reader is typing has not been said yet, and without a line between them a half-written
question reads as the newest thing on the transcript - the web panel draws the same separation and
gets it from a border where a terminal has to spend a row.
The heading carries the model and how many turns are on the journal, which is the one fact about a
resumed conversation that is nowhere else on the screen once its opening page has scrolled away.

A paste is a paste and not typing.
[`Tty`](crates/ui/src/terminal.rs) takes bracketed paste with raw mode and the alternate screen, so
a terminal hands a pasted block over as one event; it becomes the characters it is made of, its
newlines among them, and never an Enter.
Without that, every newline in a pasted stack trace is a question, and forty lines are forty turns
against a model that charges for each of them.
`Line` keeps the breaks - what is sent is what was pasted, and the journal records it - and draws
them as spaces, because the prompt is one row and a line feed written into it lands the rest of the
prompt over whatever the view drew underneath.
The row is measured the way it is drawn, so the cursor sits where the reader sees it rather than a
column short for every break.
The prompt on the reader's own scrollback takes the same mode, for the same reason.
`tetanus chat` ([crates/cli/src/chat.rs](crates/cli/src/chat.rs)) is that same live view, asked for
again after every answer: one engine over one journal, and a loop that reads a line, runs a turn and
comes back for the next.
It holds no conversation of its own, because `TurnEngine` derives each request's history from the
journal it was built on - which is why leaving and resuming is not a different thing from never
leaving, and why the loop is this small.
The three things it prints that a turn does not - the opening page, the prompt marker, the card of
commands - are in [crates/cli/src/render/chat.rs](crates/cli/src/render/chat.rs); everything between
them is the `Reader` above.
A typed line is read by one pure function, so what counts as a command is a unit test rather than a
session driven through a pty.

`tetanus replay` reads a finished journal through that same `Reader`, printed whole, played back at
the pace it happened (`--live`), or on a page of its own (`--ui`,
[crates/cli/src/render/browse.rs](crates/cli/src/render/browse.rs)).
It is handed a path or an id, and a target that is nothing on disk is looked for under the settled `sessions.root` as `<root>/<target>.jsonl` - the path a store resolves an id to - so the ids `tetanus sessions` prints are the ids this command opens.
A target that is a path is opened as it was given and the settings document is not read at all: a journal the reader can see is the one they meant, whatever a document says about roots.
Its full-screen view is driven by `tetanus_ui::show` rather than by a loop of its own, which is what
a view over something already finished can do and a view over a turn in flight cannot.
The printed list keeps every id whole, because an id is the one thing on that page a reader retypes,
and stacks the row where the window has no room left for a title
([crates/cli/src/render/sessions.rs](crates/cli/src/render/sessions.rs)): the id on a line of its
own and the counters, the state and the title indented under it.
A table folded by the terminal at column zero reads as another session; two lines that say which is
which do not.
The picker does not stack - its rows are a cursor's rows, one session each, and its frame cuts what
overruns.

`tetanus sessions --ui` ([crates/cli/src/render/pick.rs](crates/cli/src/render/pick.rs)) puts a
cursor on the list that `tetanus sessions` prints, so a directory holding more journals than the
screen is read a screenful at a time rather than scrolled back through once it has all gone past.
It composes its own frame rather than reusing `Page`, because a picker's window has to follow the
cursor and a page's follows the newest line.
Enter opens the journal under the cursor in that same reader, so finding a turn and reading it are
one screen rather than two commands and a copied path.
It is one view in two states rather than two views in sequence, because entering and leaving the
alternate screen between the list and the journal is visible to the reader as their shell flashing up.
The journal is read through the closure the binary hands in, which is the read `tetanus replay`
already does, so a journal that will not open is worded once and reported on the footer rather than
costing the reader the list.
`/` narrows the list to the journals whose id or title holds what is typed, as it is typed, and
while the prompt is open every printable key belongs to it - `q` included, because a view that quit
on `q` could not be used to look for `quota`.
In a journal `/` moves the window to the line holding the word instead of narrowing to it, because a
line of a turn is not an answer without the turn around it, and `n` walks the rest of the matches.
The word is then marked wherever the page draws it, in reverse video rather than a colour
([`light`](crates/ui/src/text.rs)), because the line it lands on already carries colours of its own
and the mark has to end without ending them.
The list marks its filter the same way, on the rows it kept, so that a row says which of the two
columns a filter reads is the one holding the word.
A journal also answers `t`, which unfolds what the model thought and folds it back: the fold is a
property of the composed line rather than of the journal, so the toggle is the same recompose a
resize and a search already go through, and the reader keeps their distance from the newest line
across it.
It is offered - on the footer and on the card - only by a journal that holds thinking, because a
card is read as a promise and a key that redraws the same page is worse than a key nobody knew
about.
The turn watched live does not answer it yet: it settles its lines as they arrive rather than
keeping the events, so unfolding there is a recompose it has no store for.

A full-screen view borrows the terminal, and `Held`
([crates/ui/src/terminal.rs](crates/ui/src/terminal.rs)) gives it back on every path out of the
scope holding it, an unwind included.
A signal is not one of those paths: `SIGTERM`, `SIGHUP`, `SIGQUIT` and `SIGINT` end the process
where it stands, `Drop` never runs, and the person at the terminal keeps raw mode, the alternate
screen and a hidden cursor - a shell that echoes nothing, over a scrollback they cannot get back to,
with no way out but to type `reset` blind.
So each view hangs `when_killed` over a second handle on the same stream just before it takes the
terminal, and that watch restores and then re-raises with the default handler, so a process that was
killed still reports itself killed by the signal that did it.
Just before rather than just after: undoing a mode nothing has entered is what a terminal ignores,
and the other order leaves a gap in which the screen is entered and no watch will leave it.
The binary hangs it rather than `Held`, for the reason this crate installs no panic hook - a signal
handler is process wide, and taking a terminal in a test should not quietly register one.

What a call was given and what it produced are laid out differently, in
[`timeline.rs`](crates/cli/src/render/timeline.rs) and therefore in every view that reads through
it.
The arguments are JSON a reader checks against what they asked for, so they stay one cut line.
The result is the work of the turn - a file, a command's output, a search - so it folds to the width
under the tool's name the way a message folds under its speaker, and a result of more than
[`CAP`](crates/cli/src/render/timeline.rs) lines keeps its first eight and its last eight with a
count between them.
Sixteen and that split are upstream's, from the terminal card in `packages/client/ui-primitives`:
the tail is kept because a command's errors and its exit line are at the end of it.
The cap counts the lines the tool wrote and not the rows a terminal draws, so a reader who resizes
is told the same number about the same journal, and no cut lands inside a line - half a line, with
its other half folded away, is a line nobody can read back.

Text the harness did not write is tamed before it is sized, in
[`tame`](crates/ui/src/text.rs).
A tool's result is whatever the tool returned and a model's answer is whatever the model wrote, so
either can carry a sequence that does more than be read: `ESC [ 2 J` clears the frame it is being
drawn into, `ESC ] 0 ;` renames the window, `BEL` rings, and a colour written this way arrives even
under `--color never`, which the surface promises will write none.
An escape sequence is therefore taken out whole - it drew nothing, so nothing is what it leaves -
and a stray control character becomes a space, which keeps two words from being joined by a byte
between them; newlines survive, because they are what a paragraph is folded on.
A tab becomes the spaces that reach the next eight-column stop, counted from the start of its own
line: a tab left alone is a width the terminal decides and no renderer here can predict, and a tab
squashed to one space is a width that is predictably wrong - a Makefile, a Go file and a stack
trace are all indented with tabs, and one column per level throws away the nesting they are read
by.
It is done inside [`truncate`](crates/ui/src/text.rs) and [`wrap`](crates/ui/src/text.rs) rather
than at each renderer, because those two are exactly the functions that size foreign text, and a
sequence taken out after it was measured would already have been paid for in columns the reader
never sees.

A fold keeps the column each line was written in, and lays what folds out of a line under it.
Not every line a model writes is prose: a fenced block, a diff, a stack trace and a table carry
meaning in their leading spaces, so a fold that dropped them would be changing the answer rather
than laying it out - `    println!()` inside a function came back flush with the `fn` above it,
which is a different program to read.
The indent is put back only on a row that has something after it, because an indent alone is
trailing space that draws nothing and still spends columns.

Width is counted over what a terminal draws as one thing, not per character
([`clusters`](crates/ui/src/text.rs)).
A family emoji is three people and two joiners - six columns counted per character, two columns
drawn - so a row measured the other way is padded four columns short and every column after it on
that row lands wrong; a skin tone and a flag are the same mistake in smaller print.
It is also where a cut has to land: between clusters, never inside one, because a cut through a
join leaves a man, a woman and a girl where a family was.
Only the joins that change what is drawn are read, not the whole of UAX #29: a combining mark, a
variation selector or a skin tone belongs to the character before it, a zero-width joiner takes the
character after it, and two regional indicators are one flag.
`fit`, `light`, `plain` and `visible_width` keep the sequences they read, because those are the
theme's own.
A tool's colour is dropped rather than honoured: upstream's terminal card parses ANSI and draws the
colours it finds, but the family of sequences that carries a colour is the family that carries a
cursor move, so a filter is what this is until a parser is written - and a parser would still have
to end here for everything it refused.

Taming inside the width rules covers the text they size and nothing else, so the short values a page
draws as themselves are tamed where they are composed: a model and a tool's name, the `call_id` a
late result names, a stop reason and its veto, and an event type this build does not know
([`timeline.rs`](crates/cli/src/render/timeline.rs)), plus the type and the unreadable line in the
`--raw` view ([`raw.rs`](crates/cli/src/render/raw.rs)).
The alternative - taming inside `Theme::paint` - was rejected twice over: `paint` is a no-op when
colour is off, which is exactly the setting that promises no colour will be written, and it is also
called on lines that already hold a nested paint, whose sequences are the renderer's own and would
be stripped with the rest.
The same edit measures the room beside a name in columns rather than characters, because a name is a
value like any other and one in a wide script would otherwise push the value it labels past the
frame.

The three list pages tame the same class of value where they compose it: a provider, the models it
advertises and the variable it asks to be set, a tool, its arguments and their types
([`catalog.rs`](crates/cli/src/render/catalog.rs)); a config key and a layer this build does not
know ([`config.rs`](crates/cli/src/render/config.rs)); a session id and a state this build does not
know ([`sessions.rs`](crates/cli/src/render/sessions.rs)).
An id is the one of those most likely to have come from somewhere else: the list reads it out of a
journal's header, so it is a value the file carries and not the name of the file it was found in.
Those pages line their columns up in what a terminal draws, and a format width cannot do that:
`{:<8}` counts the characters of the value it pads, so a name in a wide script is padded as though
it were half as wide and every column after it on that row starts somewhere else.
Each page therefore writes the spaces it measured itself, and
[`Ui::field`](crates/ui/src/writer.rs) does the same for the label column it owns, which is the one
column a renderer hands over rather than composing.

A failure report is tamed at one seam rather than at each site
([`fault.rs`](crates/cli/src/render/fault.rs)).
Every arm of the wording match composes its sentence out of something the engine sent - its own
message, or a value out of the error's `data`: a path, a session id, a tool, a method, a provider,
the protocol version a server speaks - so `wording` tames what the match returned, where a code
added to the contract cannot get past it.
The way out beside it is this module's own words and is left alone.
A diagnostic longer than the terminal is folded under its own tag by the writer that draws it
([`Ui::note`](crates/ui/src/writer.rs)), with the rest of the sentence indented past `note: ` rather
than starting at column zero, where a terminal's own fold would put it: in the column a tag goes
in, reading as a second diagnostic this build wrote without one.
These are the sentences a reader meets when something has already gone wrong, and the values inside
them are cut to the width by whoever composed them, which is where the width of a value is known.

The taming seam folds the sentence onto one line, because it is drawn after the `error:` tag on a stream and
as a single row of a frame: a newline puts a second line on stderr that reads like a report of its
own - a message ending in `note: run this` would be read as this build's advice - and inside a
frame it is a line feed with no carriage return, which is the one thing
[`Frame`](crates/ui/src/frame.rs) is careful never to write.

One arm carries a string this build did not word.
`Io` is the operating system's own message, so the sentence is lowered onto the page's voice and the `(os error N)` tail is dropped: the words in front of it have already said it, the number a script reads is the exit status of §4.5's table, and a caller on the wire still gets the message whole.
The capital is lowered only when the first word is an ordinary one, because a message opening on a path, on `I/O` or on the name of an environment variable would be naming something else once it was rewritten.
Which file the failure is about is the surface's to supply: `session.create` reports what the filesystem said about a path without carrying that path, so `run` and `chat` fill in the `path` field §4.5 asks for from the journal they asked it to open ([`main.rs`](crates/cli/src/main.rs)), and one mistake reads one way whichever view met it.

Taming and that fold together are [`tame_line`](crates/ui/src/text.rs), which is what a value drawn
as one whole row goes through, and a failure is not the only one.
A journal is headed by what the reader chose - the target `replay` was handed on the command line, or
the id the session list read out of that journal's own header - so the heading is tamed at the one
place a journal is built ([`browse.rs`](crates/cli/src/render/browse.rs)), which is the constructor
both callers use.
The session list words one sentence of its own around an id, that a journal holds nothing to read
([`pick.rs`](crates/cli/src/render/pick.rs)), and a run says what it is doing on a line naming the
model a flag or a config file asked for ([`main.rs`](crates/cli/src/main.rs)).
That name is tamed for the line that says it and for the heading of the watched view, and not for
the lookup that chose the adapter: what was given is what selects, and what is drawn is what is
drawn.

A `label  value` row is one whole row as well, and two of them carry a path the user typed.
A run's closing line says where the journal went ([`main.rs`](crates/cli/src/main.rs)), and the
serving banner names the directory the work will land in
([`serve.rs`](crates/cli/src/render/serve.rs)); both came off a flag, so both are tamed where they
are composed.
Neither is cut to fit, because a path is the one value on those pages a reader copies: a terminal
folding a long one leaves it readable, and a cut one sends them back to the flag they typed it on.
[`Ui::field`](crates/ui/src/writer.rs) itself draws what it is given, the way `line` and `heading`
do - the config table and the build page hand it a row they painted themselves, and taming it there
would take that paint out along with the sequences.

A page that lists what is in a place says which place, beside its heading.
`tetanus config` names the settings document it read and `tetanus sessions` the directory it
listed, both through [`Ui::heading_at`](crates/ui/src/writer.rs) - one method rather than two
compositions, because two shapes here would read as two kinds of answer.
The place is a path off a document, an environment or a flag, so
[`place`](crates/cli/src/main.rs) makes it absolute without asking the filesystem to resolve it,
tames it, and marks it when nothing is there yet: a relative path only answers "where do I change
it" from a working directory the page never prints, and a path with nothing at it is still the file
to write.
That mark is the reason the seam is worth having, because a config page of nothing but `default`
rows and a session list with no rows read exactly the same whether the place is empty or the reader
is looking at the wrong one.
`config --defaults` keeps the bare title, since it read no document and naming one would name a
file the answer did not come from; `--json` is unchanged in both views, which is what keeps a
caller that asked for the machine form reading one object per line.

Taming can leave nothing behind: a file whose whole name is an escape sequence is a file a reader
can make, and after the fold there is no character of it to print.
A row that stopped where its value should be reads as a value the reader failed to see rather than
one there was nothing to show, and it ends in the blank space that was meant to carry the value, so
[`or_empty`](crates/ui/src/text.rs) gives that case a word.
[`Writer::field`](crates/ui/src/writer.rs) draws it muted, so it reads as this build's word and not
as the value, and the heading a chat opens with says it the same way.

The two blocks that close the root page - the examples and the environment - are composed by one
function, not written out with their own spaces
([crates/cli/src/render/help.rs](crates/cli/src/render/help.rs)).
A block handed to clap already spaced is a block clap folds at column zero when the window is
narrow, and `--adapter deepseek` arriving under `DEEPSEEK_API_KEY`, in the column where a
variable's name goes, reads as another variable.
Composed, each row is two columns while they fit and stacked under itself when they do not, and a
command too wide for the window folds under itself rather than at column zero.
The environment list is every variable the binary reads, and the case that asserts it names each
one: a user whose output came out plain, or in ASCII, or the wrong width, reads this page to find
out which variable did it.

What the binary exits with is on the page too, under `Exit status:`
([crates/cli/src/render/help.rs](crates/cli/src/render/help.rs)), because the caller of `tetanus run
|| case $? in ...` reads a number this build has nowhere else told them about. The numbers are not
this module's: `ErrorCode::exit_status` is the single source §4.5 names, so the table beside them
words each status and decides none, and a status the contract can return with no wording beside it
fails TC-CLI-HELP-8. Several codes share a status, so a row says what they have in common rather
than naming a code a reader of a help page has never met. Only `--help` carries the block; `-h` is
the summary a person skims for a flag, and a status is for the script around them.

A flag whose value the work cannot be done with is refused at the flag.
`--speed 0` makes a duration nothing can wait for; `--max-steps 0` asks for a turn that cannot
happen, because a budget is spent by taking a step and checked afterwards, so every turn takes at
least one - accepted, it writes a journal recording the step the command line said it could not
have and closes it `step budget spent`.
Both are §4.5's exit 2, refused by a `value_parser` before any work starts, which is also what puts
the sentence beside the flag that carries the mistake.

A status the caller reads has to be the same status for the same mistake.
Every flag that takes a path takes a `PathBuf`, and clap refuses an empty one before this build is
reached; the three values that stay text (`run --model`, the journal `replay` reads, and the address
`serve` binds) took the empty string and carried it further on, so one mistake had four answers: a
run announced on a model with no name, `error: no journal at` with nothing after it, `error: :
invalid socket address`, and clap's own refusal for the other two.
All five now take clap's rule ([crates/cli/src/main.rs](crates/cli/src/main.rs)) and refuse it in
clap's words with the status §4.5 gives a wrong command line.
Only the empty string is refused: a name made of spaces is a name this build cannot judge, and
refusing it would be the presentation lane deciding what a path may be.

That block folds its own rows, and clap is given the width the rest of the binary uses
([`Policy::width`](crates/ui/src/writer.rs)) so that it agrees. clap measures the terminal itself,
which is a different answer at both ends of that clamp, and a block folded twice continues in the
number column - where a wrapped meaning reads as a status whose number went missing.

The examples are laid out the same way and for the same reason. They are held as the command and
what it is for, not as a hand-aligned line, so the gap between the two columns is measured from the
widest command rather than counted by hand, and a window with no room for a second column gets each
description under its own command instead. Two columns are what makes the block scannable - the eye
runs down one or the other and never reads a line to find out which it is looking at - and clap
folding a row that does not fit continues it in the command column, where the rest of a description
reads as the start of another invocation. Under 44 columns the widest example command does not fit
on a line at all, which is the one row on the page this layout leaves to clap.

`?` puts the whole key map of whichever view is up on a screen of its own
([crates/cli/src/render/keys.rs](crates/cli/src/render/keys.rs)), and any key at all takes it down
again, so a footer with more keys than it has room for gives up its wording rather than being cut
mid-word: each view names the keys it answers, and the shared part is only the shape.
Every width these views measure is measured in columns rather than characters
([crates/ui/src/text.rs](crates/ui/src/text.rs)), because a terminal draws a CJK character in two of
them and a combining mark in none: prose folded by character count is drawn wider than the frame
holding it, the terminal folds it again where the renderer did not mean it to, and every row under
it lands in the wrong place for the rest of the screen.
A cut lands between characters for the same reason - half of a two-column character is drawn as a
replacement glyph, which is wider than the column the cut was protecting.

`tetanus run` also observes the sequence with `TurnTrace`
([crates/turn/src/trace.rs](crates/turn/src/trace.rs)), one delegating listener per documented event,
which `--trace` prints instead of the turn.
Any other consumer would attach the same way: `session/event` for durable facts, the waterfalls for
live participation.

`tetanus-engine` ([crates/engine](crates/engine)) implements the `Engine` trait, and `tetanus-rpc`
([crates/rpc](crates/rpc)) carries it: a JSON-RPC 2.0 codec with a stdio carrier and a WebSocket
carrier. `tetanus serve` hosts the stdio one, and `tetanus serve --listen` the WebSocket one. §4.8
covers the contract all three speak.

[web/app](web/app/README.md) is the second surface, and it is a page rather than a program:
`index.html` and `chat.js` speak that WebSocket carrier directly, with no build step, no framework
and no dependency, so what a reviewer opens is the file in the repository.
It makes the four calls `tetanus chat` makes - `rpc.hello`, `session.create`, `session.subscribe`
from seq 0, then one `agent.prompt` per message typed - and draws the `session/event` pushes as they
arrive, so it needs nothing from the engine that the contract does not already publish.
Subscribing from seq 0 is what makes history and live delivery one ordered channel: a reply appears
delta by delta as the model streams it, a reload continues the conversation instead of starting one,
and no page of history can race the first live push.
The transcript is the terminal's transcript - the same rows, the same order, the same closing line -
because a turn that read differently in a browser would be a second description of the same events
for a reader to reconcile.
`tetanus serve --frontend` is what serves it, and it is the harness's own host rather than a script beside it
([crates/host](crates/host), [crates/cli/src/web.rs](crates/cli/src/web.rs)).
The page goes out on the host's single fallback seat with the shell's locked semantics - a miss is
the page with 200, so a deep link belongs to the router in the browser and not to the server - and
the address of the carrier reaches it through the boot manifest an index tap writes.
That indirection is the point: the development server this replaced string-replaced a global into
the HTML as it served it, which works exactly once, because the page then only runs when served by
that one program. A manifest is a published seam any assembly can write to and the page reads as
data.

The socket is on that same server, at `/api/ws`, and that is not tidiness.
A page served from one origin and dialling another is a cross-origin WebSocket, which is the case
the carrier's own origin check exists to refuse (§4.1.2); same origin, and the check protects the
deployment instead of fighting the page.
An upgrade route is handed the raw socket with nothing read off it, so `crates/rpc` performs its own
handshake, origin check and token check exactly as it does under `tetanus serve --listen`.
That is why the carrier peeks at a request head rather than reading it, and consumes the head only
for the requests it answers itself.

The host itself ([crates/host](crates/host)) is upstream's `host/webserver` and `host/frontend-static`:
a route carrier that knows no harness concepts, serves no files and prints nothing.
What it owns is the table and the order - exact, then longest prefix, then the one fallback - because
a carrier whose answer depends on which plugin started first is one nobody can compose against, and
a duplicate path is refused at registration rather than shadowed at the first request that goes to
the wrong owner.

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

Crossing an engine type to a wire type is the engine's, and published: `convert::session_event`,
`convert::stop_reason`, and the three error mappings §4.5 names.
The binary calls them and never matches an engine enum for itself.
Two reasons, and the second is the one that bites: two tables deciding what a script acts on is one
table too many, and an engine enum has no fallback arm, so a match on one outside the engine crate
stops compiling the day the engine names a new case - after quietly deciding, until then, what that
case means to a reader.

### 4.9 Interface view - the process seam

Everything that leaves the harness goes through `tetanus-exec`, in three layers.

`proc::Command` ([crates/exec/src/proc.rs](crates/exec/src/proc.rs)) runs one command: an argv nothing
re-splits, an environment the caller listed rather than one inherited and scrubbed, a bounded
capture that keeps the tail, and a sink that is handed each piece of output as it arrives.
Every child leads its own process group, so termination is a SIGTERM to the group, a grace period,
then a SIGKILL to the group: a command that starts grandchildren and a command that traps SIGTERM
both end. When the leader exits but something it started still holds the output pipe, the group is
swept and the caller is told - otherwise an orphan would hold a turn open for ever.

`proc::Command`'s bound is not the end of what it captured. When a stream outgrows it the whole
stream is written to the storage lane's spill store
([crates/core/src/spill.rs](crates/core/src/spill.rs)) and the truncation notice a model reads
carries the locator. The file is opened on the first overflow and never before, so a command whose
output fits touches no filesystem - and because the buffer still holds everything at that instant,
what lands on disk is the complete stream. Only the producer can do this: a policy above the seam
sees a result whose beginning is already gone. A spill that fails leaves the command exactly as it
was.

`piped::PipedCommand` ([crates/exec/src/piped.rs](crates/exec/src/piped.rs)) is the other shape of
child: one this harness *talks to* rather than waits for. A protocol peer on stdio - an MCP server
today, an out-of-process hook later - is a long conversation where stdout is the wire and closing
stdin is the request to leave. It is a seam rather than a `spawn` in each consumer because of what
the seam guarantees: the peer leads its own process group and is ended over that group, so a server
that starts helpers of its own does not leave them behind. `crates/mcp` starts its servers through
it and keeps only the framing.

`hooks::ShellHookExecutor` ([crates/exec/src/hooks.rs](crates/exec/src/hooks.rs)) is the third
consumer of the same machinery, and it exists because
[crates/hooks](crates/hooks) deliberately owns no process: that crate decides which hook fires and
what its exit status means, and declares a narrow `HookExecutor` for the running. A hook is a
deployment's program rather than a model's, which is what shapes the two defaults that differ from a
tool call - the environment is a named list rather than a scrub, because nothing here is inherited
and a denylist exposes every credential added after it was written; and the timeout ceiling is the
hook protocol's ten minutes rather than the shell tool's, because clamping a configured hook to a
model command's budget would shorten it silently. Where a hook fires is the hook protocol's own
question and is not answered here.

`backend::ShellBackend` ([crates/exec/src/backend.rs](crates/exec/src/backend.rs)) is which shell a
command goes through. `Bash` and `PowerShell` ship; a backend whose binary is absent refuses,
naming the program and where it looked, and never substitutes another shell - a bash script run
under dash fails later, elsewhere, with a message about syntax.
`shell::ShellExec` resolves a request against the deployment's defaults and caps before running it,
and renders the result into upstream's markers (`[stderr]`, `[timed out after Nms]`,
`[killed by signal: X]`, `[exit code: N]`), which `shell::parse_exit` reads back out of a replayed
result.

`session::ShellSessions` ([crates/exec/src/session.rs](crates/exec/src/session.rs)) is the
persistent half: a long-lived shell reading commands from a pipe, with a per-command nonce marker
around each one so its output and its exit status are exactly attributable. The working directory
and the exported variables survive between tool calls because the process does. A shell that dies
is reported and stays dead; nothing is restarted underneath the caller, because a fresh shell in a
state the model did not create is worse than being told.

`tools::ShellTools` ([crates/exec/src/tools.rs](crates/exec/src/tools.rs)) registers what the model
calls: `shell`, and `shell_open`/`shell_run`/`shell_close`/`shell_list`. They run in the ordinary
tool pipeline - `shell` and `shell_run` are barriers, `shell_list` is parallel-safe - and they hold
the turn's own interrupt, so stopping a turn kills the command it started rather than only ending
the loop. A composition supplies that switch through `boot_with`
([crates/turn/src/boot.rs](crates/turn/src/boot.rs)); the engine mints one per session, because one
switch shared across sessions would let an interrupt in one stop another.

The fourth layer is a terminal rather than a pipe, and it exists because a program behaves
differently when it believes it has one - it colours, it pages, it prompts without echoing, and an
interactive program may refuse to run at all.
[crates/exec/src/pty.rs](crates/exec/src/pty.rs) allocates one: the child `setsid`s and takes the
slave as its controlling terminal, the size is set before anything starts, the master is drained
continuously so the kernel's buffer never blocks the child, and a signal goes to whichever process
group owns the terminal now. Closing sweeps the terminal's whole *session* rather than one process
group, because job control puts each job in a group of its own and a group kill would leave a
background `sleep` behind.

`terminal::TerminalSession` ([crates/exec/src/terminal.rs](crates/exec/src/terminal.rs)) is a shell
on one, driven one send at a time. Readiness is announced rather than guessed: the shell is told to
print an OSC 133 marker before every prompt, so a send settles when the shell says the command is
over and the marker carries its exit status. `crates/exec/src/sanitize.rs` reads that marker out of
the stream and takes the terminal's control language with it, carrying a sequence split across two
reads rather than half-printing it. Silence and an absolute deadline are the fallbacks for a program
that prints no marker, and the turn's interrupt is the fourth ending - it aims `SIGINT` at the
foreground group, so a stopped turn costs the command and not the session. `terminals::Terminals`
([crates/exec/src/terminals.rs](crates/exec/src/terminals.rs)) is who may touch which session: an
`Owner` is compared exactly, ids are never re-used, and names are unique within one owner.
`terminal_tools::TerminalTools`
([crates/exec/src/terminal_tools.rs](crates/exec/src/terminal_tools.rs)) registers the six the model
calls - `terminal_open`, `terminal_send`, `terminal_read`, `terminal_signal`, `terminal_close`,
`terminal_list` - with typing as a barrier and reading, listing and signalling parallel-safe.

**A terminal journal is a credential store.** A model answers `[sudo] password
for ci:` with an ordinary `terminal_send`, so the answer is an ordinary argument
and the journal is forever. `Tool::recorded`
([crates/turn/src/tools.rs](crates/turn/src/tools.rs)) is what the engine asks
before it appends: `terminal_send` withholds its `text` when the call sets
`secret`, `shell` and `shell_run` withhold their command line on the same flag,
and the substitution happens in all three places a call is recorded - the call,
the assistant message that carried it, and the streamed chunk. A send the model does not mark is caught by a backstop when the terminal's last
output line asked for a password: `sudo`'s mechanism, chosen for `sudo`'s
reason - it had the tty's `ECHO` flag available and built a regex over the
program's output instead, because an interactive shell holds echo off anyway
and this crate's own `stty -echo` pins it off for the session. The two rules
compose by union, never override, as the contract fixes for its own pair of
redaction rules. Neither catches a prompt worded some other way, so the floor
is also a statement, and it is in the tool descriptions where the model reads
it.

The two persistent families are not redundant. A pipe-backed session ends when a turn is stopped,
because a shell reading a pipe has nothing to interrupt; a terminal-backed one interrupts the
command and stays open. Most work wants the first, which is cheaper and needs no session to close;
anything interactive needs the second.

### 4.10 Interface view - the sandbox

`tetanus_turn::fs` fences a *path* this process was asked to open, which is a complete answer while
the code doing the opening is ours. A command a model wrote is not: it is arbitrary code, and only
the kernel can tell it no. `crates/sandbox` is that boundary, and the two are complementary.

`policy::Policy` ([crates/sandbox/src/policy.rs](crates/sandbox/src/policy.rs)) is upstream's mode
vocabulary - `read-only`, `workspace-write`, `danger-full-access` - resolved once where a call
enters and handed down whole. It carries the workspace root, the roots the mode makes writable
(including the temp areas a build actually uses, derived here so two layers cannot disagree), a
network decision, and whether partial enforcement is acceptable.

`landlock` ([crates/sandbox/src/landlock.rs](crates/sandbox/src/landlock.rs)) is the Linux backend.
The three system calls are made by hand because of the fork/exec split: the ruleset is built in the
parent, where opening directories and allocating is safe, and the child's half between `fork` and
`exec` is `prctl` plus two syscalls with no library code - after a fork in a threaded process, a
child that allocates can deadlock on a lock another thread held. Deny-by-default is the ABI's own
shape: the handled set is every right the running kernel knows, so anything no rule grants is
denied, which is why creating, removing and renaming are governed and not only writing.

Enforcement runs through both consumers of the policy. `crates/exec` applies it to processes:
`Command::confined` for one command, and one boundary per persistent shell, inherited by every
command that shell later runs. `crates/fs` applies it to the file service
([crates/fs/src/kernel.rs](crates/fs/src/kernel.rs)): Landlock restricts a thread irreversibly and
the harness must keep writing its own journal, so the boundary belongs to one worker thread that
restricts itself before accepting work, and every file operation runs there. One `Policy` value
feeds both, which is what stops "the write tool cannot write /tmp but bash can". A denial is
rendered with upstream's marker naming the mode, so a model reads policy rather than a bug in its
own command; on the file side it arrives as `FS_PERMISSION_DENIED`, the class that already meant
"the operating system refused" rather than "this build decided".

A host that cannot honour a policy refuses: `Unavailable` for a kernel without Landlock, `Degraded`
for an ABI that cannot govern what was asked, and a compile-time refusal naming the missing backend
on a platform that has none ([crates/sandbox/src/unsupported.rs](crates/sandbox/src/unsupported.rs)).
There is no path where asking for confinement and getting none is a success; the one way to run
unconfined is to write `danger-full-access`.

### 4.11 Interface view - outside the machine

Two capabilities reach past this process: a Model Context Protocol server, which is a program
somebody else wrote answering on a pipe this process owns, and an HTTP request. Both are built the
same way, and the shape is the point.

**Everything is decided above a seam.** An MCP server is a `Link`
([crates/mcp/src/link.rs](crates/mcp/src/link.rs)) - a pair of message channels - and the stdio
transport is one implementation of it; every HTTP request goes through an `HttpTransport`
([crates/web/src/http.rs](crates/web/src/http.rs)). The handshake, the revision check, paginated
discovery, the request budget, the redirect rule, the size cap, the content-type list and the
charset decode are all decisions made above those two traits, so the whole policy is asserted with
no socket in the suite and the live transports stay thin enough to read in one sitting.

**A failure out there is a failed tool call, never a failed turn.** `McpFault::class` and
`WebFault::code` name the failure on the result the model reads and the journal keeps. A server that
dies, hangs, or writes a line that is not a message ends its call with a class; the step commits and
the loop continues. TC-PORT-MCP-32 is that promise stated as behaviour against a real child process.

**Nothing this process starts outlives it.** A server is spawned with `kill_on_drop` and shut down
through a close-input, wait, kill ladder, with the departure reported rather than assumed; seven
cases spend a real child process to assert against `/proc` that it is gone, including on the path
where the handshake failed and no client was ever returned. A `Supervisor`
([crates/mcp/src/supervisor.rs](crates/mcp/src/supervisor.rs)) reconnects on a bounded budget:
delays double to a ceiling, an attempt cap ends the retrying for good, and only real uptime past
that ceiling buys a fresh budget, so a server that connects and dies four times a second exhausts
its cap rather than restarting for ever.

A server's tools reach the model through the ordinary registry, under `mcp__<server>__<raw>`
([crates/mcp/src/tools.rs](crates/mcp/src/tools.rs)), so nothing in the turn engine knows an MCP tool
from a native one. Both crates read their own section of the settings document
(`mcp.servers.<name>`, `web.tools.*`) and are composed into a harness rather than depended on by
one.

### 4.12 Interface view - the agent a session is composed from

A *preset* is a named agent: a model, a provider, a step budget, a tool subset, a prompt shape and a
persona, written inline in the settings document or in a preset directory under the same keys. A
caller names one on `session.create`; an explicit `model`, `provider` or `max_steps` on the same
call wins over what the preset says, because a caller that named both asked for that model on that
agent.

The id is resolved once, at creation, and written into the session's `session/start` header
([crates/engine/src/preset.rs](crates/engine/src/preset.rs)). A fork inherits it, and a document
edited afterwards does not move a session that is already composed: a session whose agent changed
under it half way through would leave a journal that is a record of two agents with nothing marking
the boundary. The tool subset is applied to the registry that session's turns are booted on, so the
model is never offered a tool it may not call - being offered one and refused is a step spent on a
refusal - and a preset naming a tool the harness does not have is refused where it is used rather
than quietly narrowed. The persona is a prompt section of its own at order zero, beside what plugins
contribute rather than replacing them.

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
Containment follows the same line: an observer returns nothing, so swallowing its panic loses
no answer and only spares its peers; a `serial` or `waterfall` listener returns the value the
engine acts on, so swallowing its panic would invent one.

**Our own protocol, not upstream's.** Upstream's web contract is generated from TypeScript
decorators, so it is a build artifact rather than a documented protocol and can change on any rc bump.
Adopting it would have bought a finished UI at the cost of pinning to one upstream commit. tetanus
owns its surfaces instead. This is the single largest divergence and the reason parity is functional,
not protocol-level.

## 7. Not built yet

A settings-file watcher, live subtree remount, cancellation inside a step, further adapters,
background jobs, and the WASM plugin host.
The MCP client exists over stdio (§4.11); its streamable-HTTP transport, and image and audio results
admitted into a durable attachment store, are the named follow-ups.
Agent presets are composed per session (§4.12); authoring one, and switching the preset of a running
session, are not served.
Kernel sandboxing exists for processes and for the file service (§4.10), and the per-call escalation
stamp is served through the ordinary approval gate; what is left is named in the `sandbox/*` row of
[docs/parity.md](docs/parity.md).
The file tools exist and are composed by whoever builds a registry
([crates/fs/src/tools.rs](crates/fs/src/tools.rs)); which of them the shipped binary offers by default
is the presentation lane's wiring, per §4.7's ownership table in
[docs/interface-contract.md](docs/interface-contract.md).
[README.md](README.md#current-status) has the status table; [docs/PLAN.md](docs/PLAN.md) has the phase
plan.
