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
  -> crates/turn     tetanus-turn      turn engine, events, LLM seam, tools, boot, trace
       -> crates/session  tetanus-session   durable event vocabulary + JSONL journal
       -> crates/core     tetanus-core      registry, services, event bus, effects
  -> crates/config   tetanus-config    layered config with provenance
  -> crates/fs       tetanus-fs        filesystem service, its two backends, file tools, presets
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
`tetanus-fs` depends on `tetanus-turn` and nothing depends on it: it is a *consumer* of the tool seam,
not a layer under it, which is what keeps a harness composed without file tools a harness that still
builds and runs.
It reuses the containment walk `tetanus-turn` already carries rather than growing a second one
([crates/fs/src/local.rs](crates/fs/src/local.rs)), so the fenced and unfenced backends differ only in
which root a path is judged against.
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
  conversation.
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
The default is a block under the shell prompt, redrawn in place by `Screen`.
`--ui` takes the whole terminal instead and composes each frame with `Page`
([crates/ui/src/page.rs](crates/ui/src/page.rs)), which is what makes a turn scrollable while it is
still running.
`--json` prints the contract's own result types and draws nothing.
All three read their events from the session log the engine is writing rather than from the bus: the
journal is the durable record, and polling it is what keeps the presentation lane out of the engine.
Which is also why the `--ui` view has to be told when the turn is over: a turn that fails writes no
closing event, so the only record of the failure is the value the future returned, and a view left
polling a journal that stopped growing would keep saying the turn was running.
The reason is settled onto the page in the wording [crates/cli/src/render/fault.rs](crates/cli/src/render/fault.rs)
gives every other surface, because stderr is behind the alternate screen until the reader gives up.

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
Its full-screen view is driven by `tetanus_ui::show` rather than by a loop of its own, which is what
a view over something already finished can do and a view over a turn in flight cannot.
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
It is done inside [`truncate`](crates/ui/src/text.rs) and [`wrap`](crates/ui/src/text.rs) rather
than at each renderer, because those two are exactly the functions that size foreign text, and a
sequence taken out after it was measured would already have been paid for in columns the reader
never sees.
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
The same seam folds the sentence onto one line, because it is drawn after the `error:` tag on a stream and
as a single row of a frame: a newline puts a second line on stderr that reads like a report of its
own - a message ending in `note: run this` would be read as this build's advice - and inside a
frame it is a line feed with no carriage return, which is the one thing
[`Frame`](crates/ui/src/frame.rs) is careful never to write.

Taming and that fold together are [`tame_line`](crates/ui/src/text.rs), which is what a value drawn
as one whole row goes through, and a failure is not the only one.
A journal is headed by what the reader chose - the path `replay` was handed on the command line, or
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

Taming can leave nothing behind: a file whose whole name is an escape sequence is a file a reader
can make, and after the fold there is no character of it to print.
A row that stopped where its value should be reads as a value the reader failed to see rather than
one there was nothing to show, and it ends in the blank space that was meant to carry the value, so
[`or_empty`](crates/ui/src/text.rs) gives that case a word.
[`Writer::field`](crates/ui/src/writer.rs) draws it muted, so it reads as this build's word and not
as the value, and the heading a chat opens with says it the same way.

What the binary exits with is on the page too, under `Exit status:`
([crates/cli/src/render/help.rs](crates/cli/src/render/help.rs)), because the caller of `tetanus run
|| case $? in ...` reads a number this build has nowhere else told them about. The numbers are not
this module's: `ErrorCode::exit_status` is the single source §4.5 names, so the table beside them
words each status and decides none, and a status the contract can return with no wording beside it
fails TC-CLI-HELP-8. Several codes share a status, so a row says what they have in common rather
than naming a code a reader of a help page has never met. Only `--help` carries the block; `-h` is
the summary a person skims for a flag, and a status is for the script around them.

A status the caller reads has to be the same status for the same mistake.
Every flag that takes a path takes a `PathBuf`, and clap refuses an empty one before this build is
reached; the three values that stay text (`run --model`, the journal `replay` reads, and the address
`serve` binds) took the empty string and carried it further on, so one mistake had four answers: a
run announced on a model with no name, `error: no journal at ` with nothing after it, `error: :
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

[web/chat](web/chat/README.md) is the second surface, and it is a page rather than a program:
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
`web/chat/serve.py` is the development server that opens it: it starts `tetanus serve --listen`,
reads the bound address out of the banner, and hands it to the page.

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
Containment follows the same line: an observer returns nothing, so swallowing its panic loses
no answer and only spares its peers; a `serial` or `waterfall` listener returns the value the
engine acts on, so swallowing its panic would invent one.

**Our own protocol, not upstream's.** Upstream's web contract is generated from TypeScript
decorators, so it is a build artifact rather than a documented protocol and can change on any rc bump.
Adopting it would have bought a finished UI at the cost of pinning to one upstream commit. tetanus
owns its surfaces instead. This is the single largest divergence and the reason parity is functional,
not protocol-level.

## 7. Not built yet

A settings-file watcher, live subtree remount, cancellation inside a step, further adapters, MCP,
kernel sandboxing, the web UI, and the WASM plugin host.
The file tools exist and are composed by whoever builds a registry
([crates/fs/src/tools.rs](crates/fs/src/tools.rs)); which of them the shipped binary offers by default
is the presentation lane's wiring, per §4.7's ownership table in
[docs/interface-contract.md](docs/interface-contract.md).
[README.md](README.md#current-status) has the status table; [docs/PLAN.md](docs/PLAN.md) has the phase
plan.
