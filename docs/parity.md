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
| `core/*` (agent-loop, session, tools, agent, system-prompt, scope) | 58 | Turn engine, session log, registry, four dispatch modes, prompt assembly as a waterfall, a configured tool order with a rest entry for what it does not name, resume from a cold journal, interrupt at the step boundary, a failing tool contained as a result the model reads | Fork, scoped stores, tool permissions, a queued inbox with steering and latched wakes, cancellation inside a step, a `max-tokens` stop reason, a durable `turn/end` for a failed turn, header metadata (cwd, parent session, subagent origin, delegation depth), fiber disposal, containment of a throwing decision listener (an observer is contained; `serial` and `waterfall` stay loud by design), prompt variables with `{{name}}` interpolation, runtime-context providers, scoped prompt layers, a settings key that configures the tool order, property tests | ② (port list in section 4) |
| `session/*`, `session-query/*` | 38 | JSONL log, session store, self-describing journal, crash repair on reopen, a listing that names the root it could not read | SQLite persistence, projections and caches, titles, telemetry, stats, checkpoints, log export and query | ② for persistence, ③ for query |
| `llm/*` | 34 | DeepSeek adapter, streaming seam, token-free mock, a bounded retry policy with jitter resolved out of the settings document, the executor that runs it against a live route, heuristic token pricing and the priced surface, a stream that ends without `[DONE]` refused, a completed response with no content refused as a retryable failure | Further providers, a policy set per provider rather than once for the engine, a measurement anchored on real provider usage, the three token projections | ② |
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
| `settings/*`, `boot/*` | 15 | Layered config with provenance, the settings document under the harness home, its re-read at run time, boot resolution, the engine's own settings resolved out of the document | A file watcher to drive the re-read, per-namespace schemas, redaction, writing the document back | ② |
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
| `core/agent-loop/tests/tool-order.spec.ts` | `crates/turn/tests/upstream_loop.rs` | Canonical tool order whatever the registration order, and what a configured order does instead | ported: TC-PORT-LOOP-5, -9, -10. Upstream asserts on the logged `request/header` as well as the dispatched request; tetanus logs no header, so the request is the whole assertion |
| `core/system-prompt/tests/tool-order.spec.ts` | `crates/turn/tests/upstream_loop.rs` | The rest entry, the placement it settles, and every order it refuses | ported: TC-PORT-LOOP-9..13. Upstream can only find an unregistered name while a turn assembles, because its plugins register tools later, and that turn closes with no step; a tetanus registry is settled first, so `ToolOrder::new` refuses the order and no engine is built. Upstream's stable sort between two tools sharing a name is unrepresentable in a registry keyed by name. No settings key configures an order yet |
| `core/agent-loop/tests/tool-calls.spec.ts` | `crates/turn/tests/upstream_tool_calls.rs` | Grouping, barriers, the parallel cap, and results committed in model order | ported: TC-PORT-TOOL-1..5. Upstream's two reclassification cases are not restated: they change a tool's mode while a step is running, and tetanus fixes the registry at boot as an immutable `Arc` |
| `core/agent-loop/tests/cancel.spec.ts` | `crates/engine/tests/interrupt.rs` | Interrupt during a step, and the state the session is left in | part ported: TC-PORT-CANCEL-1..5, with TC-AGENT-8 and TC-AGENT-9 pinning two more. An interrupt lands at the step boundary, so it cannot skip a tool already dispatched, and it does not relabel a turn whose last step already answered |
| `core/agent-loop/tests/resume.spec.ts` | `crates/engine/tests/resume.rs` | Resuming a session from its log | part ported: TC-PORT-RESUME-1..3. Upstream's file is mostly its agent factory - identity registration, abort signals, transactional setup and rollback - which tetanus does not have, so those cases have nothing to restate |
| `core/agent-loop/tests/contract-regressions.spec.ts` | `crates/turn/tests/upstream_regressions.rs` | The named regressions upstream keeps pinned | part ported: TC-PORT-REG-1..4, with TC-AGENT-6 pinning one more. The rest of upstream's file is about surfaces tetanus has not built - fiber disposal, a steering inbox, a `finish {kind:error}` chunk - so those cases have nothing to restate |
| `core/agent-loop/tests/contract-regressions.spec.ts` (`plugin exceptions are contained`) | `crates/core/tests/containment.rs` | An observer with a bug cannot take the engine down with it | part ported: TC-PORT-CONTAIN-1..5 at bus level, TC-PORT-REG-4 at turn level. Upstream also contains a throwing decision listener and ends that turn with an error; tetanus keeps `serial` and `waterfall` loud, so those cases wait on a durable `turn/end` for a failed turn |
| `core/session/tests/session.spec.ts`, `surface.spec.ts` | `crates/session/tests/upstream_session.rs`, `crates/turn/tests/upstream_history.rs` | The append-only log contract, replay, citations, and what derives to a message | ported: TC-PORT-SESS-1..7, TC-PORT-HIST-1..4 |
| `core/session/tests/session.spec.ts` (`SessionStore`) | `crates/engine/tests/store.rs` | Session creation, listing, and the event surface | part ported: TC-PORT-STORE-1..5, alongside TC-SESS, TC-PAGE, TC-PATH and TC-ID in `sessions.rs`. tetanus reopens a known id instead of refusing it, and has one `session.create` rather than a `prepare`/`enter`/`announce` lifecycle, so upstream's rollback, reentrancy and disposal cases have nothing to restate |
| `core/session/tests/repair.spec.ts` | `crates/turn/tests/upstream_repair.rs`, `crates/engine/tests/sessions.rs` | An interrupted turn is closed, not left dangling | ported: TC-PORT-REPAIR-1..10 for the synthesis and its commit, TC-SESS-6 for repair on reopen, TC-PORT-SESS-3 for the torn tail |
| `core/scope/tests/scope.spec.ts` | `crates/core/tests/effects.rs` | Composite teardown order, and a scope disposed more than once | part ported: TC-EFFECT-2, -3. Scope keys, scoped dispatch and the scope parent chain have no surface, so the rest of the file has nothing to restate |
| `settings/settings-file/tests/local.spec.ts` | `crates/config/tests/upstream_settings.rs` | Where the settings document lives, what parses, and what fails loud | part ported: TC-PORT-CFG-1..10 for boot and reads. Upstream's persist half - owner-only permissions, atomic replace, comment-preserving writes - has no surface, because tetanus reads the document and never writes it |
| `settings/settings-file/tests/watcher.spec.ts` | `crates/config/tests/upstream_recompose.rs` | What a re-read does to a running configuration, and what a bad one must not do | part ported: TC-PORT-CFG-11..16 for the fold, the fallback of a dropped key, the last good document surviving a bad edit, a deleted document, an unchanged re-read, and a higher layer standing. The watcher itself - its debounce, its dispose quiesce, its write path, and recovery from a watcher error - has no surface, so those cases have nothing to restate |
| `core/tools/tests/tools.spec.ts` | `crates/turn/tests/upstream_tools.rs` | An unknown tool and a tool that panics both come back as failures the model reads, and neither ends the turn | part ported: TC-PORT-TOOLS-1..5. The rest of the file is the pipeline tetanus has not built - post-execute projections, presentation metadata, permission decisions and composite dispatch - so those cases have nothing to restate. Upstream's error `code` and `name` fields have no counterpart: the `ToolError` variant is the class |
| `core/system-prompt/tests/system-prompt.spec.ts` | `crates/turn/tests/upstream_system_prompt.rs` | Assembly inputs and their order | part ported: TC-PORT-PROMPT-1..15 for section order, empty sections, waterfall composition, short-circuiting, removal on drop, one assembly per step, the named registry - explicit order, per-assembly providers, duplicate names, disposal - and a section registered as the whole prompt, restored after the waterfall. Upstream fails an assembly that finds two complete sections effective in different scopes; tetanus has no scopes, so it refuses the second registration instead. Prompt variables, runtime-context providers and scoped layers have no surface; a non-finite order is unrepresentable in an `i32` |
| `llm/llm/tests/retry-policy.spec.ts`, `llm/llm-retry/tests/retry.spec.ts` | `crates/turn/tests/upstream_retry_policy.rs`, `crates/turn/tests/upstream_retry_executor.rs` | Which failures are worth another attempt, the wait before it, and who acts on the decision | part ported: TC-PORT-RETRY-1..10 for the decision - the defaults, the bounded and unbounded modes, capped exponential backoff with symmetric jitter, and a provider-asked wait - then TC-PORT-RETRYX-1..5 for the executor that serves it, beside TC-RECOVER-1..3 for the recovery point itself. The wait is not cancellable, so a cancel that arrives during it is honoured when it ends rather than cutting it short. Upstream's validation cases are restated against the settings document as TC-RETRY-1..5 in `crates/engine/tests/retry.rs`; what is not ported is the per-provider block that owns the policy, because tetanus's document has no per-provider section yet |
| `llm/token-meter/tests/token-meter.spec.ts` | `crates/turn/tests/upstream_tokens.rs` | What content costs under the fixed heuristic, and what the surface carries | part ported: TC-PORT-TOKEN-1..10 for the density, rounding, framing per block and per role, tool calls, nested tool results, the tool catalog, a whole request, and the append-only fold. A measurement that anchors on real provider usage needs the request envelope on the log, and tetanus logs no `request/header`; replacing a range of surface nodes is compaction. Both stay phase ② |
| `llm/token-meter/tests/token-usage-projection.spec.ts`, `context-breakdown-projection.spec.ts` | none yet | Usage, pressure and breakdown projections over a session | blocked on session projections (phase ②) |
| `llm/llm-deepseek/tests/serialize.spec.ts` | `crates/turn/tests/upstream_deepseek_wire.rs` | What a message and a tool catalog look like on the official wire | part ported: TC-PORT-DS-1..4, beside TC-DS-WIRE-1 and -2. It found one defect: a tool result with no output went out as blank content instead of the sentinel upstream sends. Upstream's block-structured content - mixed text and results in one message, plugin block types, rejected images - has no counterpart in a tetanus message, and the thinking-mode fields have no surface |
| `llm/llm-deepseek/tests/translate.spec.ts` | `crates/turn/tests/upstream_deepseek_wire.rs` | What the decoder does with a frame that is short of something | part ported: TC-PORT-DS-5..9, beside TC-DS-SSE-1 and TC-DS-DECODE-1 and -2. It found one defect: a stream that never stated a finish reason reported an empty one. The empty-response classification landed with the stream that carries it (TC-DS-EMPTY-1..3, row below); the cache-hit usage fields have no surface here |
| `llm/llm-deepseek/tests/sse.spec.ts` | `crates/turn/tests/deepseek_adapter.rs` | How a stream ends, and what the sentinel closes | ported: TC-DS-CLOSE-1..4 for a stream that ends without `[DONE]`, an empty one, an unterminated `[DONE]` tail and a mid-event close; TC-DS-DECODE-3 for a frame that follows the sentinel; TC-DS-EMPTY-1..3 for a stream that ended cleanly and said nothing, against one that only reasoned and one that only called a tool; TC-DS-SSE-1 already pins event splitting and the dropped comment line. It found two defects: a cut stream returned half an answer as an answer, and a frame after `[DONE]` still appended to it. Upstream reports comment lines to a callback; tetanus has no keep-alive observer, so it drops them |
| `llm/llm/tests/api-key.spec.ts` | `crates/turn/tests/upstream_credentials.rs` | Whether a stored credential can be carried on the wire, and what to say when it cannot | ported: TC-PORT-KEY-1..6. It found one defect: the key was judged untrimmed, so a value of nothing but whitespace read as present and went to the provider. The judgement lives on the one adapter that resolves a credential; it moves to a shared seam when a second adapter needs it |

Suites deliberately not ported yet, because the surface does not exist: `fork.spec.ts`, `scoped.spec.ts` (both areas), every `properties.spec.ts`, the rest of `core/tools`, and upstream's max-tokens cases (tetanus has no `max-tokens` stop reason).
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
| 2026-08-19 | Observer panics are contained (`emit` and `parallel`) and ported (TC-PORT-CONTAIN-1..5, TC-PORT-REG-4). `serial` and `waterfall` stay loud on purpose, so the `core/*` gap now names only the throwing decision listener. |
| 2026-08-19 | The named prompt-section registry implemented (`crates/turn/src/prompt.rs`, service key `system-prompt`) and ported (TC-PORT-PROMPT-8..12). The engine's base prompt is now a registered section like any other. Variables, runtime context and scoped layers named as the remaining `system-prompt` gaps. |
| 2026-08-19 | The settings document read into the file layer (`crates/config/src/{file,home}.rs`), ported as far as reading reaches (TC-PORT-CFG-1..10). The duplicate `llm/*` row in section 3, left by two ports landing on it, folded back into one. |
| 2026-08-19 | One step's tool calls are scheduled rather than run one by one (TC-PORT-TOOL-1..5): a bounded pool for parallel-safe calls, an exclusive call as a barrier, and results committed in model order. Permissions are the remaining half of the pipeline. |
| 2026-08-19 | The settings document can be re-read at run time (`crates/config/src/recompose.rs`) and the runtime half of `watcher.spec.ts` ported (TC-PORT-CFG-11..16). A bad edit keeps the last good configuration. The watcher that would drive it is the remaining `settings/*` gap. |
| 2026-08-19 | A tool body that panics is contained as that call's failure (`crates/turn/src/tools.rs`), and the first cases ported from `core/tools` (TC-PORT-TOOLS-1..5) pin it beside the unknown-tool message. |
| 2026-08-19 | A stream that ends without `[DONE]` is refused as `PROTOCOL` (`crates/turn/src/llm/deepseek.rs`), a frame after the sentinel decodes to nothing, and `sse.spec.ts` ported (TC-DS-CLOSE-1..4, TC-DS-DECODE-3). The empty-response classification named as an `llm/*` gap that needs a contract PR before it can land. |
| 2026-08-19 | `agent/request-error` added to the turn: a failed model request is offered to a listener before it ends the turn (TC-RECOVER-1..3). It is the seam upstream's `llm-retry` package hooks; tetanus has no listener for it yet. |
| 2026-08-19 | A prompt section may declare itself the whole prompt (`Section::complete`), restored after the assembly waterfall (TC-PORT-PROMPT-13..15). Variables, runtime context and scoped layers are what is left of the `system-prompt` row. |
| 2026-08-19 | A retry policy is resolved out of the settings document (`crates/engine/src/retry.rs`, TC-RETRY-1..5): upstream's keys under `llm.retry`, upstream's rules, and every refused value naming the key that holds it. The six keys are published in the defaults layer, so `config.dump` shows a policy nobody configured. Installing the resolved policy on a route is the next step; upstream reads it from each provider's own configuration block, and tetanus's document has no per-provider section yet. |
| 2026-08-19 | The retry executor implemented (`retry::install`, on the `agent/request-error` recovery point) and ported (TC-PORT-RETRYX-1..5). Each scheduled retry is durable before its wait, so the attempt count is read back from the journal. Settings resolution is what is left of the `retry.spec.ts` row. |
| 2026-08-19 | A journal root that cannot be read is answered as one: `session.list` carries the path in its `Io` failure (contract §4.5), and only the default root reads as an empty history (TC-SESS-7..9). Before this, a root that was a file lost the path and a mistyped root reported no sessions yet. Reported by the presentation lane as issue #150. |
| 2026-08-19 | The engine resolves its own settings out of the document (`crates/engine/src/boot.rs`, TC-BOOT-1..5): the four keys `catalog::key` names, over the compiled defaults, with a value of the wrong type refused rather than ignored. Until now the reader in `crates/config` had no caller outside its own tests, so `config.dump` reported provenance for keys no document could set. Calling it from the binary is the presentation lane's wiring. |
| 2026-08-19 | A configured tool order ported (`ToolOrder`, `TOOL_ORDER_REST`, `TurnConfig::tool_order`; TC-PORT-LOOP-9..13), closing both upstream `tool-order` specs. The order is read against the registry it arranges, so a name nobody registered is refused before an engine exists rather than closing a no-step turn. A settings key for it is the remaining gap. |
| 2026-08-19 | `LlmError::EmptyResponse` ported (TC-DS-EMPTY-1..3, TC-PORT-RETRY-10): a DeepSeek completion that ends on a clean `stop` with no text, no reasoning and no tool call is a failure rather than a blank answer. `EMPTY_RESPONSE` was already in the default retryable codes with nothing able to raise it, so the defaults now match what the adapter can produce. |
