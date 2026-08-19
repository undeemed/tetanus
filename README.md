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
| Session log | Append-only JSONL journal, fsynced per append, replay verifies `seq` contiguity | Compaction, session query |
| Model providers | Deterministic offline mock; DeepSeek chat completions with SSE streaming | More adapters |
| Tools | One built-in `echo` tool through the documented pipeline, one call at a time | Shell, subprocess, filesystem, MCP client; permissions, concurrency, cancellation |
| Config | Layered resolution with provenance (`default < file < env < flag`) | Profiles, bundles, patch overlays, live recompose |
| Effects | RAII handles: dropping a registration unwinds it | Reversible effects beyond registration, live subtree remount |
| Surfaces | `tetanus` CLI, headless, and `tetanus serve`: the published contract served over the stdio and WebSocket carriers | The fire UI |
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
cargo run --bin tetanus -- run --prompt "run one full turn"
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
| `tetanus run` | Run one turn and print it as a conversation |
| `tetanus sessions` | List the journals in a directory, newest first |
| `tetanus replay <path>` | Read a session journal back, at once or `--live` |
| `tetanus models` | List providers, the models they advertise, and what is reachable |
| `tetanus tools` | List the tools an agent can call, and the arguments each takes |
| `tetanus config` | Show resolved config with its provenance layer |
| `tetanus serve` | Host the JSON-RPC protocol on stdio, or on a socket with `--listen`, for an editor or a script |
| `tetanus info` | Print what this build is: version, protocol, catalogue sizes, platform |

`tetanus run` flags: `--prompt <text>`, `--adapter mock|deepseek`, `--model <id>`,
`--session <path>`, `--max-steps <n>`, `--think` (unfold the model's reasoning),
`--trace` (the raw sequence) with `--verbose` (each durable payload), and `--json`.
`--json` is on every subcommand that makes a call, and prints that call's result type verbatim,
one JSON object per line - the shape is fixed by [docs/interface-contract.md](docs/interface-contract.md) §4.7.
Run `tetanus --help` or `tetanus run --help` for the authoritative list.

`tetanus serve` is the one subcommand that prints no page.
Its stdout belongs to the carrier, one JSON-RPC frame per line, so everything a person reads goes to stderr.
It takes `--dir <path>`, the directory the journals it writes land in.

`--listen <addr>` serves the WebSocket carrier on a socket instead of on stdio.
The banner then names the address that was bound rather than the one asked for, so `--listen 127.0.0.1:0` tells you which port the operating system chose.
That server has no end of file to stop it, so Ctrl-C is the shutdown and it exits 0.

## Workspace layout

A Cargo workspace of nine crates.

| Crate | Directory | Responsibility |
| --- | --- | --- |
| `tetanus-core` | [crates/core](crates/core) | Plugin registry, typed service registry, four-mode event bus, RAII effect handles |
| `tetanus-session` | [crates/session](crates/session) | Durable `SessionEvent` vocabulary, append-only JSONL journal, replay |
| `tetanus-turn` | [crates/turn](crates/turn) | Turn engine, live extension points, LLM adapter seam, tool registry, boot composition, tracer |
| `tetanus-config` | [crates/config](crates/config) | Layered config resolution with provenance |
| `tetanus-protocol` | [crates/protocol](crates/protocol) | The engine/presentation contract: wire types, JSON-RPC envelope, and the `Engine` facade |
| `tetanus-engine` | [crates/engine](crates/engine) | The `Engine` implementation |
| `tetanus-rpc` | [crates/rpc](crates/rpc) | The JSON-RPC codec and the stdio and WebSocket carriers |
| `tetanus-ui` | [crates/ui](crates/ui) | Terminal presentation: colour policy, theme, width, redrawable screen, and the whole-screen frame, held terminal and loop a full-screen view runs in |
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

46 tests, every one offline.
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
