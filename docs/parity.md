# Parity gaps against upstream

## 1. Identification

- **System:** tetanus (binary `tetanus`, umbrella crate `tetanus-hardness`), version 0.1.0.
- **Tracks:** deepseek-harness `0.1.0-rc.7`, commit `99f6f02fec`, consulted as a read-only reference.
- **Purpose:** name what upstream does that tetanus does not do yet, and the phase that closes each gap.
- **Status:** current as of 2026-08-19. Section 6 records every change.
- **Authoritative copy:** this file, in the tetanus repository.

This is a gap list, not a design description and not a test plan.
Design lives in [../ARCHITECTURE.md](../ARCHITECTURE.md) and [turn-flow.md](turn-flow.md).
The engine/presentation boundary lives in [interface-contract.md](interface-contract.md).
The phase plan and the parity decision live in [PLAN.md](PLAN.md).

## 2. How the numbers were taken

Upstream spec files, counted in the reference clone:

```sh
find packages apps -name '*.spec.ts' -not -path '*/node_modules/*' | wc -l   # 650
find scripts -name '*.spec.ts' | wc -l                                       # 47
```

650 spec files across 49 package families, plus 47 specs that test upstream's own build and release tooling.
tetanus today carries 103 identified cases (`TC-*`), all of which run offline.
Case counts are not a parity metric on their own: one upstream spec file can hold thirty assertions, and tetanus asserts a whole event sequence in one case.
They are used here only to size an area.

Parity is functional, not protocol-level ([../ARCHITECTURE.md](../ARCHITECTURE.md) section 6).
Upstream's web contract is generated from TypeScript decorators, so its client specs do not port one for one.

## 3. Areas

`Today` is what tetanus serves on `master`.
`Closes in` is the phase from [PLAN.md](PLAN.md): ② is Cordis parity, ③ is the WASM host and the rest.

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `core/*` (agent-loop, session, tools, agent, system-prompt, scope) | 58 | Turn engine, session log, registry, four dispatch modes | Cancel, resume, fork, scoped stores, the full tool pipeline (permissions, concurrency), property tests | ② (port list in section 4) |
| `session/*`, `session-query/*` | 38 | JSONL log, session store, self-describing journal | SQLite persistence, projections and caches, titles, telemetry, stats, checkpoints, log export and query | ② for persistence, ③ for query |
| `llm/*` | 34 | DeepSeek adapter, streaming seam, token-free mock | Further providers, retry policy, token metering | ② |
| `client/*` | 125 | Terminal UI, owned by the presentation lane | Not a port. See section 5 | out of scope |
| `host/*` (apiproxy, webserver, static frontend, directory picker) | 33 | None | HTTP host and its provider proxy | ③ |
| `subagent/*` (drivers: in-process, fork, ACP, Codex, Claude Code) | 28 | None | Subagent spawn, control and report tools | ② |
| `sandbox/*` (policy, local, Windows ACL) | 21 | None | Sandbox policy and its platform backends | ③ |
| `fs/*` (fs, local, sandboxed, tools, observation policy) | 20 | None | Filesystem service and its tools | ② with the tool pipeline |
| `hooks/*` (protocol, Claude Code, Codex) | 18 | In-process event bus with mode enforcement | Out-of-process hook protocol and its adapters | ② |
| `shell/*`, `terminal/*`, `subprocess/*` | 32 | None | Bash and PowerShell backends, persistent shells, terminal tools | ② |
| `extensions/*` (Cordis host and client runners) | 14 | Compile-time registry instead | Out-of-tree plugins need a host | ③ (WASM component host) |
| `workflow/*`, `schedule/*`, `jobs/*` | 24 | None | Workflow workers, scheduling, job store | ③ |
| `lsp/*` | 12 | None | LSP stdio client and its tool | ③ |
| `compaction/*` | 12 | None | Context compaction and the tool-result pruner | ② |
| `web/*` (fetch, search providers) | 11 | None | Web fetch and search tools | ② |
| `typert/*` | 11 | Hand-written contract in `crates/protocol` | Not a port. See section 5 | out of scope |
| `preset/*` (agent presets, persona) | 9 | None | Named agent presets | ② |
| `interaction/*` (approvals, questions, commands, permission presets) | 9 | None | Permission gates on the tool pipeline, user questions | ② |
| `settings/*`, `boot/*` | 15 | Layered config with provenance, boot resolution | Settings files, recompose at run time | ② |
| `acp/*`, `sdk/*`, `api/*` | 17 | Own JSON-RPC contract in `crates/protocol`, carriers in progress | ACP bridge, SDK client, gateway | ③ |
| `storage/*`, `spill/*`, `credentials/*` | 15 | Environment credentials for one provider | Key-value stores, spill policy, credential store | ② |
| `context/*`, `guard/*` | 8 | None | Time, tmux and instruction context; timeout and repeat guards | ② |
| `skill/*`, `todo/*`, `goal/*`, `plan/*`, `feedback/*`, `attachment/*`, `workspace/*` | 32 | None | The built-in feature tools | ② |
| `mcp/*` | 4 | None | MCP client | ② |
| `code-runtime/*`, `e2b/*` | 11 | None | Worker-thread code runtime, remote sandbox backend | ③ |
| `test-support/*`, `bundle/*`, `examples/*`, `util/*`, `identity/*`, `runtime-diagnostics/*` | 32 | Conformance harness with its own mock adapter | Mostly upstream plumbing. The LLM mock server and replay already have tetanus equivalents | out of scope |
| `apps/cli` | 7 | `tetanus run`, plus the presentation lane | Subcommand parity, per [interface-contract.md](interface-contract.md) section 4.7 | ② |
| `scripts/*` | 47 | None | Upstream's own build, lint and release gates | out of scope |

## 4. Next ports

These upstream suites test behaviour that tetanus surfaces today, so they port now rather than after a phase lands.
Each row becomes one slice, and closing it updates the matching row in section 3.

| Upstream spec | Ports to | Asserts |
| --- | --- | --- |
| `core/agent-loop/tests/loop.spec.ts` | `crates/turn/tests/turn_flow.rs` | The loop-back after a tool call, and where the turn stops |
| `core/agent-loop/tests/tool-calls.spec.ts`, `tool-order.spec.ts` | `crates/turn/tests/turn_flow.rs` | Tool call ordering, and results returned in request order |
| `core/agent-loop/tests/cancel.spec.ts` | `crates/engine/tests/` | Interrupt during a step, and the state the session is left in |
| `core/agent-loop/tests/resume.spec.ts` | `crates/engine/tests/` | Resuming a session from its log |
| `core/agent-loop/tests/request-error.spec.ts` | `crates/turn/tests/deepseek_adapter.rs` | Provider failure surfaces as a turn error, not a panic |
| `core/agent-loop/tests/interception.spec.ts` | `crates/core/tests/event_modes.rs` | A listener changing the outcome, per dispatch mode |
| `core/agent-loop/tests/contract-regressions.spec.ts` | `crates/turn/tests/turn_flow.rs` | The named regressions upstream keeps pinned |
| `core/session/tests/session.spec.ts`, `surface.spec.ts` | `crates/engine/tests/sessions.rs` | Session creation, listing, and the event surface |
| `core/session/tests/repair.spec.ts` | `crates/engine/tests/sessions.rs` | A truncated or corrupt log is repaired, not fatal |
| `core/system-prompt/tests/system-prompt.spec.ts` | `crates/turn/tests/turn_flow.rs` | Assembly inputs and their order |

Suites deliberately not ported yet, because the surface does not exist: `fork.spec.ts`, `scoped.spec.ts` (both areas), every `properties.spec.ts`, and all of `core/tools`.
They are listed with their phase in section 3.

## 5. Deliberately out of scope

- **`client/*` (125 specs).** Upstream's client is a web UI bound to a generated protocol. tetanus owns its surfaces and its UI is not a port of theirs, so these specs measure a product decision rather than a gap.
- **`typert/*` (11 specs).** Type generation for that same protocol. `crates/protocol` is written by hand and tested by `crates/protocol/tests/wire.rs`.
- **`scripts/*` (47 specs).** Upstream's build, lint and release gates. tetanus has its own, in `.github/workflows` and [../CONTRIBUTING.md](../CONTRIBUTING.md).
- **`bundle/*`, `examples/*`, most of `test-support/*`.** Packaging and demos.

Out of scope means it does not gate parity.
It does not mean the capability is refused: if one of these turns into a real requirement, it arrives as a phase item in [PLAN.md](PLAN.md) first.

## 6. Changelog

| Date | Change |
| --- | --- |
| 2026-08-19 | First list, against upstream `0.1.0-rc.7` (`99f6f02fec`). |
