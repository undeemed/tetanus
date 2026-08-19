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
tetanus's own cases each carry a `TC-*` identifier and all of them run offline.
Their count moves with every merge, so it is not kept here as a number: it is what `grep -rhoE 'TC-[A-Z0-9-]+' crates/*/tests | sort -u | wc -l` prints.
Case counts are not a parity metric on their own: one upstream spec file can hold thirty assertions, and tetanus asserts a whole event sequence in one case.
They are used here only to size an area.

Parity is functional, not protocol-level ([../ARCHITECTURE.md](../ARCHITECTURE.md) section 6).
Upstream's web contract is generated from TypeScript decorators, so its client specs do not port one for one.

## 3. Areas

`Today` is what tetanus serves on `master`.
`Closes in` is the phase from [PLAN.md](PLAN.md): ② is Cordis parity, ③ is the WASM host and the rest.

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `core/*` (agent-loop, session, tools, agent, system-prompt, scope) | 58 | Turn engine, session log, registry, four dispatch modes, prompt assembly as a waterfall, resume from a cold journal, interrupt at the step boundary | Fork, scoped stores, the full tool pipeline (permissions, concurrency), a queued inbox with steering and latched wakes, cancellation inside a step, a `max-tokens` stop reason, a durable `turn/end` for a failed turn, header metadata (cwd, parent session, subagent origin, delegation depth), isolation of a failing `session/event` observer from its peers, fiber disposal, containment of a throwing listener, a named section registry with prompt variables and runtime-context providers, property tests | ② (port list in section 4) |
| `session/*`, `session-query/*` | 38 | JSONL log, session store, self-describing journal, crash repair on reopen | SQLite persistence, projections and caches, titles, telemetry, stats, checkpoints, log export and query | ② for persistence, ③ for query |
| `llm/*` | 34 | DeepSeek adapter, streaming seam, token-free mock, a bounded retry policy with jitter | Further providers, the executor that runs the retry policy against a live route, resolving a policy out of settings, token metering | ② |
| `llm/*` | 34 | DeepSeek adapter, streaming seam, token-free mock, heuristic token pricing and the priced surface | Further providers, retry policy, a measurement anchored on real provider usage, the three token projections | ② |
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
A port restates the upstream case against the tetanus seam that carries the same decision; it is not a transcription.
Closing a row updates it here and in section 3, in the same PR.

| Upstream spec | Ports to | Asserts | State |
| --- | --- | --- | --- |
| `core/agent-loop/tests/loop.spec.ts` | `crates/turn/tests/upstream_loop.rs` | The tool result reaches the next request; `agent/pre-step` fires once per proposed step with its coordinates; an empty assembly omits the system message; a replayed journal derives the same history | ported: TC-PORT-LOOP-1, -2, -4, -8 |
| `core/agent-loop/tests/interception.spec.ts` | `crates/turn/tests/upstream_loop.rs` | A rewritten claim is what gets recorded; a refused call never runs and the model is told why | ported: TC-PORT-LOOP-3, -6 |
| `core/agent-loop/tests/request-error.spec.ts` | `crates/turn/tests/upstream_loop.rs` | A provider failure ends that turn and no more | ported: TC-PORT-LOOP-7 |
| `core/agent-loop/tests/tool-order.spec.ts` | `crates/turn/tests/upstream_loop.rs` | Canonical tool order, whatever the registration order | part ported: TC-PORT-LOOP-5. A configured `toolOrder` has no surface yet |
| `core/agent-loop/tests/tool-calls.spec.ts` | `crates/turn/tests/` | Grouping, barriers, the parallel cap, and results committed in model order | blocked on the tool pipeline (phase ②) |
| `core/agent-loop/tests/cancel.spec.ts` | `crates/engine/tests/interrupt.rs` | Interrupt during a step, and the state the session is left in | part ported: TC-PORT-CANCEL-1..5, with TC-AGENT-8 and TC-AGENT-9 pinning two more. An interrupt lands at the step boundary, so it cannot skip a tool already dispatched, and it does not relabel a turn whose last step already answered |
| `core/agent-loop/tests/resume.spec.ts` | `crates/engine/tests/resume.rs` | Resuming a session from its log | part ported: TC-PORT-RESUME-1..3. Upstream's file is mostly its agent factory - identity registration, abort signals, transactional setup and rollback - which tetanus does not have, so those cases have nothing to restate |
| `core/agent-loop/tests/contract-regressions.spec.ts` | `crates/turn/tests/upstream_regressions.rs` | The named regressions upstream keeps pinned | part ported: TC-PORT-REG-1..3, with TC-AGENT-6 pinning one more. Most of upstream's file is about surfaces tetanus has not built - fiber disposal, a steering inbox, a `finish {kind:error}` chunk, and containment of a throwing listener - so those cases have nothing to restate |
| `core/session/tests/session.spec.ts`, `surface.spec.ts` | `crates/session/tests/upstream_session.rs`, `crates/turn/tests/upstream_history.rs` | The append-only log contract, replay, citations, and what derives to a message | ported: TC-PORT-SESS-1..7, TC-PORT-HIST-1..4 |
| `core/session/tests/session.spec.ts` (`SessionStore`) | `crates/engine/tests/store.rs` | Session creation, listing, and the event surface | part ported: TC-PORT-STORE-1..5, alongside TC-SESS, TC-PAGE, TC-PATH and TC-ID in `sessions.rs`. tetanus reopens a known id instead of refusing it, and has one `session.create` rather than a `prepare`/`enter`/`announce` lifecycle, so upstream's rollback, reentrancy and disposal cases have nothing to restate |
| `core/session/tests/repair.spec.ts` | `crates/turn/tests/upstream_repair.rs`, `crates/engine/tests/sessions.rs` | An interrupted turn is closed, not left dangling | ported: TC-PORT-REPAIR-1..10 for the synthesis and its commit, TC-SESS-6 for repair on reopen, TC-PORT-SESS-3 for the torn tail |
| `core/scope/tests/scope.spec.ts` | `crates/core/tests/effects.rs` | Composite teardown order, and a scope disposed more than once | part ported: TC-EFFECT-2, -3. Scope keys, scoped dispatch and the scope parent chain have no surface, so the rest of the file has nothing to restate |
| `core/system-prompt/tests/system-prompt.spec.ts` | `crates/turn/tests/upstream_system_prompt.rs` | Assembly inputs and their order | part ported: TC-PORT-PROMPT-1..7 for section order, empty sections, waterfall composition, short-circuiting, removal on drop, and one assembly per step. The named section registry, prompt variables, runtime-context providers and complete sections have no surface |
| `llm/llm/tests/retry-policy.spec.ts`, `llm/llm-retry/tests/retry.spec.ts` | `crates/turn/tests/upstream_retry_policy.rs` | Which failures are worth another attempt, and the wait before it | part ported: TC-PORT-RETRY-1..9 for the defaults, the bounded and unbounded modes, capped exponential backoff with symmetric jitter, and a provider-asked wait. The executor half - the attempt loop, its abort signal and the events it publishes - has no surface yet, and no policy is resolved out of settings, so upstream's validation cases have nothing to restate |
| `llm/token-meter/tests/token-meter.spec.ts` | `crates/turn/tests/upstream_tokens.rs` | What content costs under the fixed heuristic, and what the surface carries | part ported: TC-PORT-TOKEN-1..10 for the density, rounding, framing per block and per role, tool calls, nested tool results, the tool catalog, a whole request, and the append-only fold. A measurement that anchors on real provider usage needs the request envelope on the log, and tetanus logs no `request/header`; replacing a range of surface nodes is compaction. Both stay phase ② |
| `llm/token-meter/tests/token-usage-projection.spec.ts`, `context-breakdown-projection.spec.ts` | none yet | Usage, pressure and breakdown projections over a session | blocked on session projections (phase ②) |
| `llm/llm-deepseek/tests/serialize.spec.ts` | `crates/turn/tests/upstream_deepseek_wire.rs` | What a message and a tool catalog look like on the official wire | part ported: TC-PORT-DS-1..4, beside TC-DS-WIRE-1 and -2. It found one defect: a tool result with no output went out as blank content instead of the sentinel upstream sends. Upstream's block-structured content - mixed text and results in one message, plugin block types, rejected images - has no counterpart in a tetanus message, and the thinking-mode fields have no surface |
| `llm/llm-deepseek/tests/translate.spec.ts` | `crates/turn/tests/upstream_deepseek_wire.rs` | What the decoder does with a frame that is short of something | part ported: TC-PORT-DS-5..9, beside TC-DS-SSE-1 and TC-DS-DECODE-1 and -2. It found one defect: a stream that never stated a finish reason reported an empty one. The empty-response classification, `STREAM_CLOSED` for a stream that ends without `[DONE]`, and the cache-hit usage fields have no surface here |
| `llm/llm/tests/api-key.spec.ts` | `crates/turn/tests/upstream_credentials.rs` | Whether a stored credential can be carried on the wire, and what to say when it cannot | ported: TC-PORT-KEY-1..6. It found one defect: the key was judged untrimmed, so a value of nothing but whitespace read as present and went to the provider. The judgement lives on the one adapter that resolves a credential; it moves to a shared seam when a second adapter needs it |

Suites deliberately not ported yet, because the surface does not exist: `fork.spec.ts`, `scoped.spec.ts` (both areas), every `properties.spec.ts`, all of `core/tools`, and upstream's max-tokens cases (tetanus has no `max-tokens` stop reason).
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
| 2026-08-19 | First seven cases ported from `agent-loop` into `crates/turn/tests/upstream_loop.rs`. Two gaps they exposed added to the `core/*` row. |
| 2026-08-19 | The log contract and history derivation ported from `core/session` (TC-PORT-SESS-1..7, TC-PORT-HIST-1..4, TC-PORT-LOOP-8). Crash repair named as the remaining `repair.spec.ts` gap. |
| 2026-08-19 | Crash-repair synthesis implemented (`crates/turn/src/repair.rs`) and ported (TC-PORT-REPAIR-1..9). |
| 2026-08-19 | The closers are committed when a cold journal is reopened (TC-PORT-REPAIR-10, TC-SESS-6), closing the `repair.spec.ts` row. |
| 2026-08-19 | System-prompt assembly ported as far as the surface reaches (TC-PORT-PROMPT-1..7). It found one defect: `SystemPrompt::text()` did not drop empty sections. The section registry, variables and context providers added to the `core/*` gap. |
| 2026-08-19 | Resume ported as far as the surface reaches (TC-PORT-RESUME-1..3): a continued journal, the carried transcript, and repair that happens once. |
| 2026-08-19 | Cancellation ported as far as the surface reaches (TC-PORT-CANCEL-1..5). The queued inbox, steering and in-step cancellation it does not cover moved into the `core/*` gap column. |
| 2026-08-19 | The `SessionStore` half of `session.spec.ts` ported as far as the surface reaches (TC-PORT-STORE-1..5). Header metadata and observer isolation added to the `core/*` gap column. |
| 2026-08-19 | The named regressions ported as far as the surface reaches (TC-PORT-REG-1..3): publication order, tool-call identity, and request routing. Disposal and listener containment added to the `core/*` gap column. |
| 2026-08-19 | Effects compose: `EffectScope` unwinds newest first, nests, and finishes past a panicking undo, and `Registry::start_all` rolls a failed mount back (TC-EFFECT-1..6, TC-PLUGIN-1..2). |
| 2026-08-19 | The retry policy implemented (`crates/turn/src/llm/retry.rs`) and ported (TC-PORT-RETRY-1..9). The executor that runs it and settings resolution named as the remaining `llm/*` gap. The section 2 case count was stale at 103; it is now a command to run rather than a number to maintain. |
| 2026-08-19 | Heuristic token pricing and the priced surface implemented (`crates/turn/src/tokens.rs`) and ported (TC-PORT-TOKEN-1..10). The usage anchor, compaction's surface replacement and the three projections named as the remaining `token-meter` gaps. |
| 2026-08-19 | The DeepSeek wire contract ported as far as the surface reaches (TC-PORT-DS-1..9). It found two defects: a tool result with no output went out blank, and a stream that stated no finish reason reported an empty one. |
| 2026-08-19 | Credential judgement ported (TC-PORT-KEY-1..6) and normalized before use. It found one defect: a blank stored key read as present and spent a real request to be refused. |
