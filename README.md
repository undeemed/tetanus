# tetanus

Rust rewrite of deepseek-harness. Everything it has, but better — because it's in Rust.

- **binary:** `tetanus`
- **publishable umbrella crate:** `tetanus-hardness` (bare `tetanus` is squatted on crates.io)
- **member crates:** `tetanus-core` (plugin registry / event bus / RAII effects), `tetanus-config` (layered config w/ provenance), `tetanus-session` (append-only JSONL journal + replay), `tetanus-turn` (turn engine), `tetanus-hardness` (CLI)
- **spec:** docs/PLAN.md (captain-approved decision doc, 2026-08-18) · docs/plan-visual.html (diagram diff)

Phases: ① core turn engine → ② Cordis parity (reversible effects, live remount, WASM host) → ③ better (conformance suite, perf proof, fire UI).
