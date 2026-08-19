# Contributing to tetanus

Thanks for working on tetanus.
This file covers dev setup, the commands CI runs, code style, and what a reviewable PR looks like.
For what the project is and where it is going, read [README.md](README.md) and
[docs/PLAN.md](docs/PLAN.md) first.

## Dev setup

You need a stable Rust toolchain with `rustfmt` and `clippy`.
Development happens on Rust 1.97; no minimum supported version is declared yet.

```bash
rustup toolchain install stable
rustup component add rustfmt clippy
git clone https://github.com/undeemed/tetanus.git
cd tetanus
cargo build --workspace
```

No API key, no service account, and no network access is needed to build, run, or test.

## The commands CI runs

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs these four, in this order, with
`RUSTFLAGS: -D warnings`. Run them locally before you push and CI will not surprise you.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo build --workspace
cargo test --workspace
```

CI sets no `DEEPSEEK_API_KEY`, so every case in the merge gate runs offline.

## Running it

```bash
cargo run --bin tetanus -- run --prompt "run one full turn"
cargo run --bin tetanus -- replay sessions/turn.jsonl
```

`sessions/` is gitignored. `--adapter deepseek` needs `DEEPSEEK_API_KEY` and is the only thing here
that touches the network.

## The conformance suite

The suite is the merge gate. It is ordinary `cargo test`, so `cargo test --workspace` runs it.

```bash
cargo test -p tetanus-turn --test turn_flow      # the documented sequence
cargo test -p tetanus-core --test event_modes    # the four dispatch modes
cargo test -p tetanus-turn --test boot           # registry composition
cargo test -p tetanus-protocol --test wire       # the engine/presentation contract
cargo test -p tetanus-hardness --test run_offline
cargo test -p tetanus-turn --test properties   # what holds for every journal
```

Three rules keep it a gate rather than a formality.

1. **The expected sequence is one constant.** `MOCK_TURN_FLOW` in
   [crates/turn/tests/harness/mod.rs](crates/turn/tests/harness/mod.rs) holds the whole event
   sequence of one turn and is asserted by equality. Changing the driver's event order means editing
   that constant on purpose, in the same commit, with the reason in the commit message and in
   [docs/turn-flow.md](docs/turn-flow.md).
2. **Every case runs offline.** No case may need an API key. `TC-DS-LIVE-1` is the one live provider
   case and reports itself skipped when `DEEPSEEK_API_KEY` is absent. Keep it that way.
3. **One tracer, two readers.** `TurnTrace` ([crates/turn/src/trace.rs](crates/turn/src/trace.rs))
   feeds both `tetanus run` and the suite, so the printed sequence and the asserted sequence cannot
   drift. Do not add a second observer.

Every test case carries a stable identifier (`TC-TURN-1`, `TC-BUS-WATERFALL-2`, `TC-DS-SSE-1`, ...)
in its doc comment, with its expected result stated. New cases get a new identifier in the same
family and a row in the verification table of the design document they cover. "It passes" is not an
expected result.

A property case states what holds for every input rather than for one:
[crates/turn/tests/properties.rs](crates/turn/tests/properties.rs) generates journals and asserts
the invariants the turn engine reads them back under. It carries a `TC-PROP-*` identifier and an
expected result like any other case. When one fails, proptest writes the shrunken counterexample to
a `*.proptest-regressions` file beside the suite; check that file in with the fix, so the input that
found the defect runs first from then on.

## Code style

- **Formatting is `cargo fmt --all`.** Default rustfmt settings, no local overrides. CI checks it.
- **Clippy is clean at `-D warnings`** across `--all-targets`. Fix the lint rather than allowing it;
  if an `#[allow]` is genuinely right, say why in a comment next to it.
- **Doc comments carry the contract.** Every public type and module says what it is for and, where it
  matters, which upstream document it answers to. Look at
  [crates/core/src/events.rs](crates/core/src/events.rs) for the bar.
- **Name no concrete implementation from the engine.** Components resolve through the typed service
  registry. Swapping an adapter, tool set, or journal is a boot-time change.
- **Errors are typed.** `thiserror` in libraries, `anyhow` at the binary edge.
- Read [AGENTS.md](AGENTS.md) before your first change. It lists the sharp edges that are easy to
  "fix" by accident.

## Documentation style

- Design documents follow IEEE 1016: identification, stakeholders and their concerns, design views,
  and rationale. [docs/turn-flow.md](docs/turn-flow.md) is the worked example. A design document
  without rationale is a diagram.
- Test documentation follows IEEE 829 proportionately: stable case identifiers and explicit expected
  results, as above.
- In Markdown, put each full sentence on its own line. It keeps diffs readable.
- Use a plain dash, never an em dash.
- Do not add a badge for a service that is not set up, or claim a feature that is not built. The
  status table in [README.md](README.md) is truthful and stays that way.

## Pull requests

- Branch off `master`. Never push to `master` and never merge your own PR.
- **Keep a PR under 500 changed lines** of reviewable source and docs, additions plus deletions.
  Lockfiles, generated artifacts, vendored code, and pure renames do not count; say so in the body
  when you rely on that. If a change does not fit, stack it: contracts and types first, then
  implementation, then callers, then docs. Every PR in the stack must build and test on its own.
- One concern per PR. Drive-by refactors belong in their own PR.
- The PR body says what changed, why, and how you verified it. If the event sequence moved, say which
  events and why.
- Update the docs in the same PR as the code. A behaviour change that leaves
  [ARCHITECTURE.md](ARCHITECTURE.md) or [docs/turn-flow.md](docs/turn-flow.md) stale is incomplete.
- A change to the engine/presentation boundary is its own PR. It touches
  [docs/interface-contract.md](docs/interface-contract.md) and `crates/protocol` together, adds a
  changelog row, and lands before anything that depends on it.

## Commit hygiene

Subject line: lowercase, `area: what changed`, imperative, under ~72 characters.
Areas in use so far: `phase ①`, `docs`, `test`, `scaffold`, `rename`.

```text
docs: turn-flow design description + "run one turn" in the README
test: turn event sequence conformance
```

The body explains why, and groups per-crate detail under a bare crate name when a commit spans
several. `git log` has the worked examples.

Commits are logically self-contained: each one builds and tests green.
Do not add an agent as a co-author.
