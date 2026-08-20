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
| Session log | Append-only JSONL journal, fsynced per append, replay verifies `seq` contiguity; a session forks at a closed turn boundary into a child that continues the conversation it inherited | Compaction, session query |
| Model providers | Deterministic offline mock; DeepSeek chat completions with SSE streaming; a bounded retry policy for transient failures, with the executor that runs it against a live route and records every scheduled attempt; heuristic pricing of the model-visible surface | More adapters, token counts anchored on what a provider reports, the usage and context projections |
| Tools | One built-in `echo` tool through the documented pipeline; parallel-safe calls share a bounded pool, an exclusive call is a barrier, results commit in model order | Shell, subprocess, filesystem, MCP client; permissions, cancellation |
| Config | Layered resolution with provenance (`default < file < env < flag`), reading a `settings.yaml` or `.json` document under the harness home, re-reading it at run time, and resolving the engine's own settings out of it, which every subcommand that builds an engine boots from, with the flags it was given over it on the `flag` layer | Profiles, bundles, patch overlays, a file watcher |
| Effects | RAII handles and scopes: unwinding is newest-first, nests, and finishes past a panicking undo; a failed plugin mount rolls boot back | Live subtree remount |
| Surfaces | `tetanus` CLI, headless, with `--ui` for a scrollable full-screen view of a turn - live, replayed, or picked off the session list; `tetanus chat` for a conversation of many turns on one journal; `tetanus serve`: the published contract served over the stdio and WebSocket carriers; `web/chat`, a browser panel that holds a conversation over that WebSocket carrier | The fire UI |
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
It lands at `sessions/turn.jsonl` under the current directory unless `--session <path>` says
otherwise. Read it back with:

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
| `tetanus replay <path>` | Read a session journal back: at once, `--live`, or full-screen with `--ui` |
| `tetanus models` | List providers, the models they advertise, and what is reachable |
| `tetanus tools` | List the tools an agent can call, and the arguments each takes |
| `tetanus config` | Show resolved config with its provenance layer |
| `tetanus serve` | Host the JSON-RPC protocol on stdio, or on a socket with `--listen`, for an editor or a script |
| `tetanus info` | Print what this build is: version, protocol, catalogue sizes, platform |

`tetanus run` flags: `--adapter mock|deepseek`, `--model <id>`,
`--session <path>`, `--max-steps <n>`, `--think` (unfold the model's reasoning),
`--trace` (the raw sequence) with `--verbose` (each durable payload), `--ui` (a screen of its own),
and `--json`.
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

`tetanus replay <path> --ui` reads a finished journal the same way and with the same keys, so a long turn is paged through instead of poured into the scrollback, which it leaves untouched.
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
So is what a view is headed with and what a run says it is doing: the path you typed, the id read out of a journal's header, and the model a flag or a config file named are drawn as their words, on the one row each is given.
This holds in every view and under every `--color` setting.
`--raw` prints the payload as the JSON it is, where an escape is six characters no terminal acts on.

`tetanus chat` holds a conversation instead of running one turn: type a message, watch the turn arrive, and the prompt comes back for the next one.
Every exchange is appended to one journal, and each turn is asked with the ones before it as history, so the conversation remembers for as long as the journal does.
That journal is `sessions/chat.jsonl` unless `-s/--session <path>` says otherwise, and a path that already holds a conversation is resumed rather than replaced: the opening page says how many turns it is carrying, and the next turn is numbered after them.
It takes the same `--adapter`, `--model`, `--max-steps` and `--think` flags as `run`, and like `run` it defaults to DeepSeek, so a chat with no `DEEPSEEK_API_KEY` says so and stops before a journal is opened.
A line that opens with a slash is a command: `/help` lists them, `/exit` leaves, and `//text` asks the model `/text` rather than running it as one.
Ctrl-D leaves the way `/exit` does and exits 0, Ctrl-C stops what is running and exits 130, and either way every turn already written stays on the journal.
Standard input can be a pipe as well as a keyboard - `tetanus chat -a mock < questions.txt` asks each line in turn - and a piped chat prints the transcript without the prompt marker.

`tetanus serve` is the one subcommand that prints no page.
Its stdout belongs to the carrier, one JSON-RPC frame per line, so everything a person reads goes to stderr.
It takes `--dir <path>`, the directory the journals it writes land in.

`--listen <addr>` serves the WebSocket carrier on a socket instead of on stdio.
The banner then names the address that was bound rather than the one asked for, so `--listen 127.0.0.1:0` tells you which port the operating system chose.
That server has no end of file to stop it, so Ctrl-C is the shutdown and it exits 0.

[web/chat](web/chat/README.md) is a browser panel over that carrier: a page and a script, no build step, that holds the same conversation `tetanus chat` holds and draws each reply as it streams.
`python3 web/chat/serve.py` starts a `tetanus serve` behind it and prints the address to open.

## Workspace layout

A Cargo workspace of nine crates.

| Crate | Directory | Responsibility |
| --- | --- | --- |
| `tetanus-core` | [crates/core](crates/core) | Plugin registry, typed service registry, four-mode event bus, RAII effect handles |
| `tetanus-session` | [crates/session](crates/session) | Durable `SessionEvent` vocabulary, append-only JSONL journal, replay |
| `tetanus-turn` | [crates/turn](crates/turn) | Turn engine, live extension points, LLM adapter seam, tool registry, boot composition, tracer |
| `tetanus-config` | [crates/config](crates/config) | Layered config resolution with provenance, and the settings document it reads |
| `tetanus-protocol` | [crates/protocol](crates/protocol) | The engine/presentation contract: wire types, JSON-RPC envelope, and the `Engine` facade |
| `tetanus-engine` | [crates/engine](crates/engine) | The `Engine` implementation |
| `tetanus-rpc` | [crates/rpc](crates/rpc) | The JSON-RPC codec and the stdio and WebSocket carriers |
| `tetanus-ui` | [crates/ui](crates/ui) | Terminal presentation: colour policy, theme, width, redrawable screen, the whole-screen frame and the scrollable page on it, held terminal and loop a full-screen view runs in |
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
