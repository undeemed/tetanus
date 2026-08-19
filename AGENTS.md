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
- **Phase boundaries.** Layered config recompose, deeper reversible effects, the full tool pipeline, more adapters, the JSON-RPC/WebSocket surfaces and the WASM host are Phase ②/③. `docs/PLAN.md` is the captain-approved decision doc.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.
