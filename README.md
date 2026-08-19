# tetanus

Rust rewrite of deepseek-harness. Everything it has, but better - because it's in Rust.

- **binary:** `tetanus`
- **publishable umbrella crate:** `tetanus-hardness` (bare `tetanus` is squatted on crates.io)
- **member crates:** `tetanus-core` (plugin registry / event bus / RAII effects), `tetanus-config` (layered config w/ provenance), `tetanus-session` (append-only JSONL journal + replay), `tetanus-turn` (turn engine), `tetanus-hardness` (CLI)
- **spec:** docs/PLAN.md (captain-approved decision doc, 2026-08-18) - docs/plan-visual.html (diagram diff) - docs/turn-flow.md (turn-flow design description)

Phases: ① core turn engine → ② Cordis parity (reversible effects, live remount, WASM host) → ③ better (conformance suite, perf proof, fire UI).

## Run one turn

Phase ① runs one full documented turn headlessly.
The default adapter is a deterministic built-in mock, so this needs no API key and no network.

```bash
cargo build --workspace
cargo run --bin tetanus -- run --prompt "run one full turn"
```

It prints the event sequence the turn emitted, then the outcome:

```text
   0     0  turn/start
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
The second is the journal sequence number, and it is blank for the live extension points, which are dispatched but never persisted.

The session journal is append-only JSONL.
It lands at `sessions/turn.jsonl` under the current directory unless `--session <path>` says otherwise.
Read it back with:

```bash
cargo run --bin tetanus -- replay sessions/turn.jsonl
```

Useful flags: `--adapter mock|deepseek`, `--model <id>`, `--session <path>`, `--max-steps <n>`, `--verbose` (print each durable payload).
`--adapter deepseek` needs `DEEPSEEK_API_KEY`; without it the command says so and stops before any network call.
`DEEPSEEK_BASE_URL` overrides the endpoint.

## Test

```bash
cargo test --workspace
```

Every case runs offline.
The one live provider case (TC-DS-LIVE-1) reports itself skipped unless `DEEPSEEK_API_KEY` is set.

