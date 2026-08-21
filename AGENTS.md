# Project agent memory

This file is the project's committed home for project-intrinsic agent knowledge: build, test, release, architecture, and sharp-edge notes that should travel with the code.

- Add durable project-specific notes here as they are discovered through real work.

## Sharp edges

- **Names are settled.** The binary is `tetanus`; the publishable umbrella crate is `tetanus-hardness`, because bare `tetanus` is squatted on crates.io. Do not rename either.
- **Upstream disagrees with itself about `system-prompt/assemble`.** `architecture.md` puts prompt assembly before `agent/pre-step`; `agent-lifecycle.md` names the event and puts it inside the step. tetanus follows agent-lifecycle.md. The reasoning is in `docs/turn-flow.md` section 6.1; read it before "fixing" the order.
- **Four dispatch modes, not five.** Upstream's low-level API doc lists a `bail` mode; the harness primer documents four to plugin authors, and Phase ① implements those four. `bail` is reachable through serial.
- **The merge gate is the conformance suite.** `MOCK_TURN_FLOW` in `crates/turn/tests/harness/mod.rs` is the whole expected event sequence, asserted by equality. Changing the driver's event order means changing that constant on purpose, in the same commit, with a reason.
- **Every test runs offline.** No case may need an API key. The one live provider case reports itself skipped when `DEEPSEEK_API_KEY` is absent. Keep it that way, or CI stops being a gate.
- **One tracer, two readers.** `TurnTrace` (`crates/turn/src/trace.rs`) feeds both `tetanus run` and the conformance suite, so the printed sequence and the asserted sequence cannot drift. Do not add a second observer.
- **The engine<->presentation boundary is published, not implied.** `docs/interface-contract.md` is the spec; `crates/protocol` is the same contract as Rust types. Any change to either lands as its own PR touching both plus the doc's changelog, never inside a feature PR. The doc's file-ownership table says which lane owns which path.
- **Phase boundaries.** Layered config recompose, deeper reversible effects, the full tool pipeline, more adapters, a subcommand that hosts the WebSocket carrier, and the WASM host are Phase ②/③. `docs/PLAN.md` is the captain-approved decision doc. The JSON-RPC codec and both its carriers are served: `crates/rpc`, stdio and WebSocket, conformance in `crates/rpc/tests/`.
- **Docs are part of done.** `README.md` has a truthful status table, `ARCHITECTURE.md` is the workspace design description, `CONTRIBUTING.md` holds the dev commands. A behaviour change that leaves them stale is unfinished; `CONTRIBUTING.md` states the standards they follow.
- **There are two gates, and the second one is `sentrux gate`.** It reads the committed `.sentrux/baseline.json` and prints an absolute quality score: hold 7000+, floor 6200, and quote it in the pull request body. It scans `git ls-files`, so an uncommitted or untracked change is invisible to it: a new crate measures identically to `master` until it is committed. Commit the slice, then gate it, or the number is about somebody else's tree. **Never `sentrux gate --save`** - that overwrites the thing the branch is being measured against, so a branch that saves reports "no degradation" by construction. `CONTRIBUTING.md` has the reasoning; refreshing the baseline is its own PR naming the commit measured.
- **A green test count is not proof the gate ran your code.** Confirm a case that exists only in your tree actually appears in the run. Every lane must also point `CARGO_TARGET_DIR` at its *own* directory: cargo does not hash the final binary, every lane writes `debug/tetanus`, and the CLI tests exec that exact path through `env!("CARGO_BIN_EXE_tetanus")`, so two lanes sharing one target directory can run each other's binary and still pass.
- **`docs/parity-changelog.md` is append-only and merged, not adjudicated.** It is a separate file marked `merge=union` in `.gitattributes` because every slice in flight appends one row and git merges by line, which made a queue of four slices produce four identical conflicts per merge. Rows are historical facts: write one, never revise one - a correction is a new row saying what it corrects. Do not widen the pattern; union merge never reports a conflict, so `docs/parity.md` and `docs/interface-contract.md`, which are edited in place, deliberately keep ordinary merge semantics.
- **A compaction record and the event after it are one unit.** `compaction/summary` and `compaction/prune` name a range and price it; the *next* surface event replaces that range. Nothing may be appended between the two. It is not tidiness: it is what lets a projection price a replacement with one running total and one pending claim instead of a price per message, so a checkpoint stays bounded. `crates/turn/src/compaction.rs` produces the pair and `crates/turn/src/projections.rs` consumes it; break the adjacency and the totals drift silently.

- **Compaction lives in the derivation, never in the file.** The journal is append-only, so nothing is deleted to shrink a conversation - `compaction::surface` changes how history is *derived*, which is why a replay reproduces a compacted session. Anything that reads model history must go through `derive_messages`; a second reader that folds surface events itself will disagree with the engine the first time a session compacts.

- **TC-ENG-4 and TC-RPC-12 belong to whichever call is reserved *now*.** Contract section 4.2's `Reserved` status is served by a default trait body on `Engine`; those two cases assert that a reserved call answers `NotImplemented`, is routed rather than unknown, and is not advertised. Serving a call *moves* them to the next reserved call rather than retiring them, and the RPC routing arm is added by hand, so the arm that gets forgotten is the one no case names.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
