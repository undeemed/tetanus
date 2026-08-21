# tetanus

A Rust agent harness that runs the DeepSeek Harness turn flow, headlessly and offline by default.

`tetanus` is a ground-up Rust rewrite of [deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness).
The goal is 1:1 functional parity with upstream, plus a CLI and UI worth using.
It is not a port: no upstream source was translated.
Upstream's published design documents are the specification, and this repository owns its own implementation.

**Status: Phase ①, pre-alpha.**
One full documented turn runs end to end.
Most of upstream's surface is not built yet.
[Current status](#current-status) says exactly what works today.

## Table of contents

- [Why](#why)
- [Current status](#current-status)
- [Install from source](#install-from-source)
- [Quickstart](#quickstart)
- [CLI](#cli)
- [Workspace layout](#workspace-layout)
- [Documentation](#documentation)
- [Testing](#testing)
- [Contributing](#contributing)
- [License](#license)

## Why

Upstream is roughly 568K lines of TypeScript over ~50 workspace packages, built on the
[Cordis](https://github.com/cordiverse/cordis) plugin runtime.
Its architecture is good: everything is a plugin, effects are reversible, the session log is
append-only, and the turn flow is specified rather than implied.
Its runtime is not: wiring errors surface mid-run, the web protocol is a build artifact of TypeScript
decorators rather than a documented contract, and the whole thing needs Node.

tetanus keeps the architecture and changes the runtime.

- **Wiring is checked at compile time and at boot.** A service is a type, not a string key. A missing
  provider fails at boot naming the service, not on the first turn.
- **A dispatch mode is part of an event's contract.** Dispatching an event through the wrong mode
  panics instead of silently doing nothing.
- **The documented event sequence is the merge gate.** It is asserted whole, by equality, by the
  conformance suite.
- **One self-contained binary.** No Node, no `node_modules`, no runtime to install.

The full option analysis and the scope decision live in [docs/PLAN.md](docs/PLAN.md).

## Current status

Phase ① is the core turn engine. It is implemented and covered by tests.

| Area | Today | Planned |
| --- | --- | --- |
| Turn flow | The complete documented sequence, `turn/start` through `turn/end`, driven end to end | Continuation after a stop veto |
| Extension points | Eight live extension points in a turn, over the four dispatch modes upstream documents | Capability seams (`fs/*`, telemetry) |
| Session log | Append-only JSONL journal, fsynced per append, replay verifies `seq` contiguity; a session forks at a closed turn boundary into a child that continues the conversation it inherited; a SQLite store behind the same seam, chosen by `sessions.backend`, with a lossless migration either way; the journal read back as data - turn and step derived from the structural events, filters by turn, step, role, tool, time, outcome and text, paging by `seq`, and named aggregates for tool calls, a tool's failing turns and what a range of turns cost | Telemetry, full-text search and its cursors |
| Context | A conversation that outgrows its window folds its older span into a summary recorded on the journal, so a replay derives the compacted history; over-long tool results shrink first, without a model | Manual compaction, per-model policy |
| Projections | Named folds over the journal, driven as events commit and checkpointed: title, stats, token usage, context pressure, context breakdown | Telemetry, log export |
| Secrets and spill | A credential store outside the settings document - the environment over an owner-only file - whose values reach no dump, log line or journal; oversized payloads spill to disk behind a bounded preview | Wiring spill into the tool pipeline |
| Model providers | Deterministic offline mock; DeepSeek chat completions with SSE streaming; a bounded retry policy for transient failures, with the executor that runs it against a live route and records every scheduled attempt; heuristic pricing of the model-visible surface | More adapters, token counts anchored on what a provider reports |
| Tools | `echo`, `shell`, the four persistent-shell tools and the six terminal tools through the documented pipeline; parallel-safe calls share a bounded pool, an exclusive call is a barrier, results commit in model order; a call a tool declares irreversible is decided before it runs, and a refused call never runs; `web_fetch` and `web_search`, and the tools any MCP server advertises, all dispatched by that same pipeline. Every tool the binary offers comes from one declared set of named sources - one line per crate - which the settings document selects by crate, so the tools page and the registry a turn dispatches from are the same list | Cancellation inside a step; starting declared MCP servers at boot |
| Feature tools | The built-in tools a usable harness has, each over state kept only on the journal: a todo list, a standing goal with revisions, plan mode, an operator feedback channel, skills discovered from project and user roots, attachments admitted and content-addressed, and a workspace sketch; a surface reads all of it through one folded vocabulary (`SessionView`, `WorkspaceView`) that carries no bytes and no presentation; all of them offered by the shipped binary | Putting the views on the JSON-RPC boundary, an autonomous goal driver, a workspace picker |
| Filesystem | A filesystem service with a local and a sandboxed backend behind one trait - read, write, edit, list, glob, stat, delete - each failing in a named class rather than an `io::Error` string; seven model-facing tools over it; the read-before-write policy that refuses to overwrite what a session has not read | Read windows over bytes, a search tool, the kernel sandbox backends |
| Process execution | One command through a resolved shell backend (bash, or PowerShell where a host has it), argv without a shell re-split, an environment the caller listed, output bounded to its tail - with the whole of it kept beside the session's journal when the bound drops something, and the result saying where - and a timeout that kills the whole process group; persistent shells that keep `cd` and exported variables between tool calls and report a death rather than restarting under the caller; persistent *terminals* on a real pseudo-terminal, where a command settles when the shell announces its own prompt with that command's exit status, a `^C` reaches the command rather than the shell, a bounded scrollback pages back from the newest line, and long work is started with a short wait and collected later; one seam for a protocol peer this harness talks to over pipes, ended over its own process group so a server's helpers go with it; a credential typed at a terminal is withheld from the journal when the model marks it, and the tool descriptions say plainly that anything not marked is written down; a screen model beside the transcript, so a program that draws with cursor movement - an editor, a pager, `htop` - is read as the screen it is showing rather than as every frame it ever painted | `run_in_background` for one-shot commands, which needs a job store; a PowerShell prompt marker and a Windows host to prove the backend on |
| Sandboxing | A deployment writes `sandbox.mode` (with `sandbox.workspace` and `sandbox.network`) in its settings document and every child the binary starts obeys it - commands, persistent shells, terminals and hooks - because the engine settles one policy value and the composition hands that value to each; enforced by Landlock on both sides: a spawned command confined between `fork` and `exec`, a persistent shell confined once for every command it will run, and the file service confined on a worker thread that restricts itself - so a path the fence allows and the policy does not is refused by the kernel; a denial told to the model as policy rather than as a bug in its command; a host that cannot enforce what was asked refusing at composition rather than pretending | A `--sandbox` flag beside the settings key, a Windows ACL backend, changing the mode inside a session, parallel file operations behind the boundary |
| Permissions | A tool call gated on a decision, audited on the journal as one `approval/asked`/`approval/decided` pair; user questions with the same durable pair; presets bundling the filesystem mode and the approval policy under one name | Serving the decision over the boundary (`approval.set`, `ui/approve`, `ui/ask`) |
| Outside the machine | An MCP client over stdio - handshake, discovery, invocation, a shutdown that leaves no child behind, and a bounded reconnect supervisor; a fetch with redirect, size and content-type policy above one transport seam; a search seam with one provider and a deterministic mock. A server that dies, hangs or answers nonsense fails that tool call with a class, and the turn carries on | Streamable HTTP for MCP, attachments for image results, more search providers, HTML converted to markdown |
| Code runtime | A model-written program evaluated once, on a worker thread, under fuel, a wall-clock ceiling and one output ledger; a runaway loop is stopped and its thread reclaimed. A program can call the harness's own tools - several inside one turn step - and can `catch` one that failed, while its own budget stays uncatchable. A remote backend behind the same trait submits, polls, fetches and cancels. Registered as `run_code` by whoever composes a registry, and configured from the settings document, so a program that fails is a failed tool call and the turn survives | Offering it from the shipped binary, which needs a `crates/toolset` source that can see the assembled registry; a real-language backend, which waits on the sandbox modes; OOM containment, which needs a per-worker heap cap |
| Agent presets | A preset names a model, a tool subset, a prompt shape and a persona, written inline in the settings document or as a preset directory, selected per session on `session.create`, recorded in the session header and inherited by a fork | Authoring presets, switching one on a running session, a `--preset` flag on `tetanus run` |
| Config | Layered resolution with provenance (`default < file < env < flag`), reading a `settings.yaml` or `.json` document under the harness home - or the one `--settings <path>` names - re-reading it at run time, and resolving the engine's own settings out of it, which every subcommand that builds an engine boots from - the provider, model, step budget and journal root a turn runs on included - with the flags it was given over it on the `flag` layer | Profiles, bundles, patch overlays, a file watcher |
| Background work | A durable job store with the journal's crash discipline; workflow runs of declared steps, cancellable at a boundary and resumable from the record; schedules on an anchored grid, with an explicit answer for a fire that overlaps a run | Model-facing tools over all three; delivering a fired schedule into a session |
| Language intelligence | An stdio language-server client - bounded framing, a real handshake and query lifecycle, bounded waits - and the `lsp` tool over it for definitions, references and diagnostics; a server that dies is a failed call, not a dead turn | A pool of servers reused across calls || Effects | RAII handles and scopes: unwinding is newest-first, nests, and finishes past a panicking undo; a failed plugin mount rolls boot back | Live subtree remount |
| Surfaces | `tetanus` CLI, headless, with `--ui` for a scrollable full-screen view of a turn - live, replayed, or picked off the session list; `tetanus chat` for a conversation of many turns on one journal, and `tetanus chat --ui` for that conversation held on a screen of its own - transcript above, the line you type on pinned to the foot of it; `tetanus serve`: the published contract served over the stdio and WebSocket carriers; `tetanus serve --frontend`, the browser panel served by the harness's own HTTP host, with the protocol on the same address | A history to walk with the up and down keys; a turn stopped without leaving the chat |
| Contract surfaces | An ACP bridge riding the JSON-RPC carrier - initialize, `session/new`, `session/prompt`, one-way cancel, the turn as `session/update` - and an ACP client that spawns one and drives a whole turn over pipes, answering its permission questions; an in-process SDK that drives the same turn with no CLI and no socket; the request surface as an enumerable catalog that validates named arguments before dispatch | Image and audio prompts, ACP session load/fork/resume, an engine-side approval seam for the permission channel |
| Plugins | Compile-time composition through a typed registry | WASM component host for out-of-tree plugins |

Phase boundaries are set in [docs/PLAN.md](docs/PLAN.md); what Phase ① deliberately left as a seam is
listed in [docs/turn-flow.md](docs/turn-flow.md) section 7.

This repository is private and nothing is published to crates.io.
Build from source.

## Install from source

Requires a stable Rust toolchain.
Development happens on Rust 1.97; no minimum supported version is declared yet.

```bash
git clone https://github.com/undeemed/tetanus.git
cd tetanus
cargo build --workspace
```

The binary lands at `target/debug/tetanus`.
Use `cargo build --workspace --release` for an optimised build at `target/release/tetanus`.

## Quickstart

The default adapter is a deterministic built-in mock, so a full turn needs no API key and no network.

```bash
cargo run --bin tetanus -- run "run one full turn"
```

It prints the turn as a conversation, then where the journal went.
`--trace` replaces that view with the raw event sequence:

```text
   0     1  turn/start
   1        agent/pre-step
   2     1  step/start
   3     2  user/message
   4        system-prompt/assemble
   5        agent/request
   6        llm/stream
   7     3  assistant/chunk
  ...
  27        agent/turn-stopping
  28    16  turn/end

turn    1
steps   2
stop    natural
journal sessions/turn.jsonl

You said: run one full turn
```

The first column is the position in the sequence.
The second is the journal sequence number, blank for the live extension points, which are dispatched
but never persisted.

The session journal is append-only JSONL.
Its first line is a `session/start` header naming the id, the provider, the model and the step
budget, so a reader can open a journal nobody told them about.
It lands at `turn.jsonl` under `sessions.root` - `sessions` under the current directory unless the
settings document says otherwise - and `--session <path>` overrides both. Read it back with:

```bash
cargo run --bin tetanus -- replay sessions/turn.jsonl
```

Model history is derived from the journal, so a second run against the same path continues the same
conversation rather than starting over.
Point `--session` at a fresh path for an independent turn.

To talk to the real provider, set `DEEPSEEK_API_KEY` and pass `--adapter deepseek`.
Without the key the command says so and stops before any network call.
`DEEPSEEK_BASE_URL` overrides the endpoint.

## CLI

| Command | What it does |
| --- | --- |
| `tetanus run` | Run one turn and print it as a conversation, or watch it full-screen with `--ui` |
| `tetanus chat` | Hold a conversation: one journal, a turn per message you type, resumed by `--session` |
| `tetanus sessions` | List the journals in a directory, newest first, or pick one to read with `--ui` |
| `tetanus replay <journal>` | Read a session journal back - by path, or by the id `tetanus sessions` printed: at once, `--live`, or full-screen with `--ui` |
| `tetanus models` | List providers, the models they advertise, and what is reachable |
| `tetanus tools` | List the tools an agent can call, and the arguments each takes |
| `tetanus config` | Show resolved config with its provenance layer, or `--defaults` for what this build compiles in |
| `tetanus serve` | Host the JSON-RPC protocol on stdio, or on a socket with `--listen`, for an editor or a script |
| `tetanus info` | Print what this build is: version, protocol, catalogue sizes, platform |

`tetanus run` flags: `--adapter mock|deepseek`, `--model <id>`,
`--session <path>`, `--max-steps <n>`, `--think` (unfold the model's reasoning),
`--trace` (the raw sequence) with `--verbose` (each durable payload), `--ui` (a screen of its own),
and `--json`.
`--settings <path>` and `--color <when>` are on every subcommand, before it or after it.
The first names the settings document to read in place of the one under the harness home (`$TETANUS_HOME`, or `~/.tetanus`), and a path with nothing there is an error rather than a quiet fall back to the compiled defaults.
Which document a run read is on the `tetanus config` heading, written out in full and marked when nothing is there yet, so "where do I change it" is answered by the page rather than by the flags you typed; `--defaults` names none, because it read none.
`tetanus sessions` heads its list the same way, with the directory it listed, whether `--dir`, the settings document or the compiled default chose it.
`--json` is on every subcommand that makes a call, and prints that call's result type verbatim,
one JSON object per line - the shape is fixed by [docs/interface-contract.md](docs/interface-contract.md) §4.7.
Run `tetanus --help` or `tetanus run --help` for the authoritative list.

A script reads the exit status rather than the page: `0` when it did what was asked, `2` for a wrong command line, `4` for something named that is not there or is busy, `5` for a credential that is not set, `6` for a provider that refused, `130` for an interrupt, and `1` for a fault of this build's own.
`tetanus --help` words all of them under `Exit status:`, and the numbers are [docs/interface-contract.md](docs/interface-contract.md) §4.5's, which every surface tetanus ships exits with.
`-h` leaves the block out, because it is the summary you skim for a flag.

The prompt is the command's own argument: `tetanus run "list the files"`.
`-p/--prompt` takes the same text and cannot be combined with it.
Either form given as `-` reads the prompt from standard input instead, so `tetanus run - < task.md` and a heredoc both become one turn, newlines and all.
With no prompt at all the run asks `run one full turn`, which is what makes the quickstart above a bare command.
An empty prompt is refused before the journal is opened, with the exit status [docs/interface-contract.md](docs/interface-contract.md) §4.5 gives a bad argument.

`--ui` watches the turn on a screen of its own instead of in a block under the shell prompt.
Up and Down move a row, PageUp and PageDown a screenful, Home goes to the first line of the turn and End back to following the newest, and `q`, Esc or Ctrl-C closes the view.
`?` spells out every key the view you are in answers, and any key goes back to what you were reading; on a terminal too narrow for the whole footer, `? keys` and the way out are what is kept.
The transcript is kept whole, so a turn can be read back from its first line while it is still running, and the block showing what is arriving stays at the foot of the screen however far back you have scrolled.
The view outlives the turn: it stays up until you close it, because the moment to look back over a turn is after it has finished.
When the turn is over the block goes and the footer reads `end`, and a turn that failed says why on the page it failed on rather than only on the way out.
Closing it before the turn finishes stops the turn, and a stopped turn has no result to report.
It needs a terminal - a piped `--ui` is refused with the same exit status as any other bad argument, before a journal is opened - and it cannot be combined with `--trace` or `--json`.
When the view comes down, the answer and the journal path are written on the ordinary screen, so what a run leaves in the scrollback is the same either way.

`tetanus replay` takes either a path or the id `tetanus sessions` listed the journal under, so an id read off that page can be typed straight back in.
A path that is there is opened as it was given; an id is looked for under `sessions.root`, and `--dir <path>` says where instead.

`tetanus replay <journal> --ui` reads a finished journal the same way and with the same keys, so a long turn is paged through instead of poured into the scrollback, which it leaves untouched.
Nothing is arriving in that view, so the foot of the screen says `end` rather than `live`, and a terminal made narrower rewraps the journal rather than cutting it.
Rewrapping counts the columns a terminal draws, not characters, so a prompt in a script drawn two columns to the character folds inside the frame like any other.
`q` or Esc leaves it having read the journal, and exits 0; Ctrl-C is an interruption, and exits 130 like any other.
It cannot be combined with `--raw`, `--live` or `--json`, and like `run --ui` it needs a terminal.

`tetanus sessions --ui` puts a cursor on that list instead of printing it, so a directory holding more journals than the screen is read a screenful at a time.
Up and Down move the cursor, PageUp and PageDown a screenful, Home and End reach the newest and the oldest, and Enter reads the journal the cursor is on with the same keys `replay --ui` uses.
`q` or Esc closes a journal back to the list and closes the list to the shell, so Enter is a key you can afford to press.
A journal that will not open says why at the foot of the list and leaves the cursor where it was; the list is not worth losing over one bad file.
`/` narrows the list to the journals whose id or title holds what you type, as you type it, and marks the word on the rows it keeps; Enter accepts the word and gives the cursor back, and Esc gives the whole list back.
Inside a journal, in `replay --ui` as well, `/` and a word move the page to the line that holds it and `n` walks the rest; the footer says which match you are on, and the word itself is marked wherever the page draws it.
`--think` unfolds the model's reasoning in whatever journal is opened, `--json` cannot be combined with it, and like the other two views it needs a terminal.
Inside a journal `t` unfolds it too, and folds it back, so a folded row saying how many lines are behind it is a row you can open rather than a reason to close the view and run the command again; a journal whose answers were not thought about does not offer the key.
What a tool produced is read rather than glimpsed: every view folds it to the width under the tool's own name, keeps the lines the tool wrote as lines, and a result longer than sixteen of them keeps its first eight and its last eight with a count of what is between - the same cap upstream's terminal card uses, so the same result folds the same way in both front ends.
The arguments of the call above it stay cut to one line, because arguments are checked rather than read.

Nothing in a journal can drive the terminal it is drawn on.
An escape sequence - a screen clear, a window title, a bell, a colour of its own - is read as the words around it and nothing else, wherever it arrived: in an answer, in a tool's result, and in the short values a page draws as themselves, such as the model, a tool's name, an event type this build does not know, or the line `--raw` could not read.
The list pages are the same: a provider and the models it advertises, a tool and its arguments, a config key and the layer that settled it, and the id of a session, which `tetanus sessions` reads out of the journal's header rather than off the file it found.
A name a terminal draws two columns to the character takes those columns in the list it is in, so every row beside it stays lined up.
A failure report is the same, and it is one line however the failure was written: the sentence is composed out of what the engine sent - a path, a session id, a tool, a method, a provider - and a second line under `error:` would read as a second report.
A failure about a file names the file and reads as one lower-case clause - `held: is a directory`, whether the journal was opened to write it or to read it - because the operating system's own capital and its `(os error 21)` tail say nothing the words in front of them have not said, and the number a script reads is the exit status.
So is what a view is headed with and what a run says it is doing: the path you typed, the id read out of a journal's header, and the model a flag or a config file named are drawn as their words, on the one row each is given.
This holds in every view and under every `--color` setting.
`--raw` prints the payload as the JSON it is, where an escape is six characters no terminal acts on.

`tetanus chat` holds a conversation instead of running one turn: type a message, watch the turn arrive, and the prompt comes back for the next one.
Every exchange is appended to one journal, and each turn is asked with the ones before it as history, so the conversation remembers for as long as the journal does.
That journal is `chat.jsonl` under `sessions.root`, unless `-s/--session <path>` says otherwise, and a path that already holds a conversation is resumed rather than replaced: the opening page says how many turns it is carrying, and the next turn is numbered after them.
It takes the same `--adapter`, `--model`, `--max-steps` and `--think` flags as `run`, and where `run` is the mock adapter when nothing says otherwise a chat is DeepSeek, so a chat with no `DEEPSEEK_API_KEY` says so and stops before a journal is opened.
A settings document that sets `provider.default` decides for both, since a flag that was not passed is not an opinion and a document is.
A line that opens with a slash is a command: `/help` lists them, `/exit` leaves, and `//text` asks the model `/text` rather than running it as one.
Ctrl-D leaves the way `/exit` does and exits 0, Ctrl-C stops what is running and exits 130, and either way every turn already written stays on the journal.
Standard input can be a pipe as well as a keyboard - `tetanus chat -a mock < questions.txt` asks each line in turn - and a piped chat prints the transcript without the prompt marker.

`tetanus chat --ui` holds the same conversation on a screen of its own.
The transcript is above and scrollable - the arrows a line at a time, the page keys a screenful, Escape back to the foot - the turn arrives in the block under it, and the row you type on is pinned to the foot of the terminal wherever you have scrolled to.
Every printable key belongs to that row, `?` and `q` included, so the commands are still the chat's own: `/help`, `/exit`, `//text` to ask a question that opens with a slash, `/find word` to look back through what was said - ctrl-n and ctrl-p walk the matches, Escape ends the search - `/keys`, which names every key the screen answers, the editing ones included, and `/think` and `/more`, which unfold what the model thought and print a tool's result whole - both toggles, over what is already on the page.
The up and down keys walk the questions you have asked, keeping whatever you were half-way through writing; the page keys scroll the conversation, and Tab and Shift-Tab walk the turns themselves - a turn's opening line to the top of the screen, which is how you get back to something said thirty turns ago without scrolling through the twenty-nine in between.
A pasted block is one question: the terminal is held in bracketed paste, so the newlines in it are text rather than forty presses of Enter, and the message keeps them.
A line finished while a turn is still being answered waits on the row rather than being sent, because a session answering one prompt refuses a second.
Ctrl-C and Ctrl-D mean what they mean in the ordinary chat, and exit with the same statuses, so a script wrapping either reads one answer.
It needs a terminal to draw on: `tetanus chat --ui | cat` is a usage error rather than a chat with nowhere to put itself.

`tetanus serve` is the one subcommand that prints no page.
Its stdout belongs to the carrier, one JSON-RPC frame per line, so everything a person reads goes to stderr.
It takes `--dir <path>`, the directory the journals it writes land in.

`--listen <addr>` serves the WebSocket carrier on a socket instead of on stdio.
The banner then names the address that was bound rather than the one asked for, so `--listen 127.0.0.1:0` tells you which port the operating system chose.
That server has no end of file to stop it, so Ctrl-C is the shutdown and it exits 0.

[web/app](web/app/README.md) is a browser panel over that carrier: a page and a script, no build step, that holds the same conversation `tetanus chat` holds and draws each reply as it streams.
`tetanus serve --listen <addr> --frontend web/app` serves it from the harness's own HTTP host, puts the protocol on the same address, and hands the page that address through the boot manifest.

## Workspace layout

A Cargo workspace of twenty-three crates.

| Crate | Directory | Responsibility |
| --- | --- | --- |
| `tetanus-core` | [crates/core](crates/core) | Plugin registry, typed service registry, four-mode event bus, RAII effect handles, the durable key-value store, the spill store, and the job and schedule stores |
| `tetanus-session` | [crates/session](crates/session) | Durable `SessionEvent` vocabulary, the JSONL and SQLite journals behind one seam, replay, and the projection registry with the units that need no pricing |
| `tetanus-turn` | [crates/turn](crates/turn) | Workflow runs, the language-server client and its tool, the turn engine, live extension points, LLM adapter seam, tool registry, the decision seams a call and a question are gated on, boot composition, tracer, compaction and the priced projections |
| `tetanus-toolset` | [crates/toolset](crates/toolset) | The one declared set of tool sources this build offers, and the settings key that selects them |
| `tetanus-fs` | [crates/fs](crates/fs) | The filesystem service, its local and sandboxed backends, the read-before-write policy, the model-facing file tools, and the permission presets over them |
| `tetanus-config` | [crates/config](crates/config) | Layered config resolution with provenance, the settings document it reads, and the credential store that deliberately is not in it || `tetanus-protocol` | [crates/protocol](crates/protocol) | The engine/presentation contract: wire types, JSON-RPC envelope, and the `Engine` facade |
| `tetanus-engine` | [crates/engine](crates/engine) | The `Engine` implementation |
| `tetanus-exec` | [crates/exec](crates/exec) | Process execution: the subprocess seam and its process-group termination, the piped seam a protocol peer is started through, the executor a configured hook runs through, the shell backends, persistent shells and terminals, and the model-facing shell and terminal tools |
| `tetanus-sandbox` | [crates/sandbox](crates/sandbox) | The sandbox policy every surface applies, and the Landlock backend that enforces it |
| `tetanus-rpc` | [crates/rpc](crates/rpc) | The JSON-RPC codec and the stdio and WebSocket carriers |
| `tetanus-host` | [crates/host](crates/host) | The HTTP route carrier the web GUI rides on: named routes, one fallback seat, upgrade seats, and the directory picker behind them |
| `tetanus-mcp` | [crates/mcp](crates/mcp) | The MCP client: a server on stdio, its tools in the registry, and the supervisor that keeps it up |
| `tetanus-coderuntime` | [crates/coderuntime](crates/coderuntime) | The code runtime: one seam for evaluating a model-written program, a worker-thread backend under fuel and a ceiling, and a remote one |
| `tetanus-web` | [crates/web](crates/web) | `web_fetch` and `web_search`, the transport seam under them, and one search provider |
| `tetanus-hooks` | [crates/hooks](crates/hooks) | The out-of-process hooks protocol: which hooks an event selects, what is written to them, and how several answers combine |
| `tetanus-subagent` | [crates/subagent](crates/subagent) | Delegation: an agent that starts another, and the budget that stops the recursion |
| `tetanus-ui` | [crates/ui](crates/ui) | Terminal presentation: colour policy, theme, width, redrawable screen, the whole-screen frame and the scrollable page on it, held terminal and loop a full-screen view runs in |
| `tetanus-features` | [crates/features](crates/features) | The built-in feature tools: skill, todo, goal, plan, feedback, attachment and workspace |
| `tetanus-query` | [crates/query](crates/query) | A session journal read as data: derived turn and step, filters, paging, and the named aggregates |
| `tetanus-sdk` | [crates/sdk](crates/sdk) | The in-process client and owned-run API, and the request surface as an enumerable catalog |
| `tetanus-acp` | [crates/acp](crates/acp) | The Agent Client Protocol, both halves - the bridge riding the JSON-RPC carrier, and the client that drives one |
| `tetanus-hardness` | [crates/cli](crates/cli) | The `tetanus` binary |

The binary is `tetanus`; the publishable umbrella crate is `tetanus-hardness`, because the bare
`tetanus` name is squatted on crates.io. Both names are settled.

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) - how the workspace fits together, with pointers into source.
- [docs/](docs/README.md) - the deeper documents, with an index of which one answers what.
  Start with [docs/turn-flow.md](docs/turn-flow.md) for the turn,
  [docs/interface-contract.md](docs/interface-contract.md) for the engine/presentation boundary, and
  [docs/PLAN.md](docs/PLAN.md) for the scope decision.
- [AGENTS.md](AGENTS.md) (also readable as `CLAUDE.md`) - sharp edges for coding agents.

## Testing

```bash
cargo test --workspace
```

Every case runs offline, and no case needs a key.
The one live provider case reports itself skipped unless `DEEPSEEK_API_KEY` is set.
The suite is the merge gate: it asserts the whole documented event sequence by equality.
[CONTRIBUTING.md](CONTRIBUTING.md) explains how to change that sequence on purpose.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for dev setup, the commands CI runs, code style, and PR
conventions.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this
project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.

Upstream deepseek-harness is MIT licensed, copyright (c) 2026 DeepSeek.
tetanus is an independent implementation written against upstream's published design documents and
carries no upstream code, so no upstream notice is required; the acknowledgement is offered anyway.
