# DeepSeek Harness → Rust: full option plan

Captain asked (2026-08-18) for a lavishly clear plan of the differences: what is possible, what isn't.
Supersedes the short scoping note (data/deepseek-harness-rust-scoping.md) as the decision doc.

**DECISION (captain, 2026-08-18): full parity — "everything deepseek-harness has, but better."**
Option 1 retired. Runtime mutability is IN scope: RAII effect handles (reversible effects), live
subtree remount from layered config, WASM component host for hot-swappable out-of-tree plugins.
The one honest residue: HMR of *native core* code — covered by ms-restart + session-log resume.
Estimate at parity scope: ~25–35K lines; the WASM host is the priciest single item, accepted.
Phases: ① core turn engine → ② Cordis parity → ③ better (WASM host, conformance suite, perf proof).

## Ground truth about the upstream (verified in /tmp/deepseek-harness clone)

- 0.1.0-rc.7, developer preview, breaking changes expected. MIT.
- ~568K TS lines, ~50 workspace packages: acp api attachment boot bundle client code-runtime
  compaction context core credentials e2b extensions feedback fs goal guard hooks host identity
  interaction jobs llm lsp mcp plan preset runtime-diagnostics sandbox schedule sdk session
  session-query settings shell skill spill storage subagent subprocess terminal test-support todo
  typert util web workflow workspace.
- **Everything is a Cordis plugin.** No privileged core: model adapter, tool registry, session log,
  and the agent loop itself are all plugins mounted into a shared context with typed events and
  *reversible effects* (registrations unwind on plugin unload, hot-module-reload supported).
  Profiles/bundles compose the boot tree from ordered config layers plus `cordis.patch.yml`
  overlays; any config row is replaceable by a user patch (`dsh --profile web --dump-config`).
- **The web API is generated, not hand-written.** Business services mark methods `@Remote` /
  `@RemoteScope`; the build generates Host and Client contracts (Typert). Calls ride a Connection
  RPC over `/api`; Host objects cross the wire via TypertLookupMap identity resolution (e.g. an
  `Agent` param becomes an `agentId` wire field resolved by the gateway). Session events and
  streaming use the same Connection but separate protocols.
- **The turn flow is cleanly specified** in docs: turn/start → claim input → assemble prompt
  sections + tool schemas → agent/pre-step → step (user/message append, derive history from log,
  agent/request → llm/stream → assistant/chunk* → assistant/message, tool pipeline
  tools/pre-execute → execute → post-execute) → step/end → … → agent/turn-stopping.
  Session log is an append-only SessionEvent log; events are the extension points
  (session/* durable, agent/* live, capability seams fs/* tools/* telemetry/*).

## What is POSSIBLE in Rust, cheaply
- The essential loop: agent loop, append-only session event log, prompt/tool-schema assembly,
  LLM adapter seam with streaming, MCP client, subprocess/shell/terminal exec, sandbox policy
  (their native helper is already landlock-based), approval/guard pipeline. Est. 15–25K Rust lines.
- The documented event taxonomy (session vs agent vs capability events) maps well to Rust
  enums + a typed event bus.
- Compile-time plugin composition: traits + feature flags + a static registry, configured by files.

## What is POSSIBLE but EXPENSIVE
- **Option 1's protocol compatibility.** The wire contract is a *build artifact* of their TS
  decorators, not a stable documented protocol. To keep their web frontend we must re-implement in
  Rust: the Connection RPC framing, `/api` route, the generated Remote method surface the web app
  actually calls, Typert identity lookup semantics, and the session-event stream. All feasible —
  but the surface must be extracted from their generated Client contracts, and every upstream rc
  bump can silently change it. Budget a protocol-extraction tool + conformance tests, and accept
  pinning to one upstream commit.
- Config layering (profiles/bundles/patch overlays): reproducible in Rust with serde + layered
  config, but "patch any row of the plugin tree" only makes sense if the tree is data, which drags
  in a mini plugin framework.

## What is NOT realistic in Rust
- ~~Cordis semantics~~ — **re-scoped to IN (captain decision 2026-08-18)**: reversible effects via
  RAII handles, live remount via tree-as-data, code hot-swap via a WASM component host. This is the
  project's biggest line item and is accepted; only TS-style HMR of native core code stays out
  (ms-restart + session-log resume stands in).
- A 1:1 port of ~50 packages (568K lines): chasing an rc-stage moving target built on a paradigm
  Rust can't express. Advise against permanently.
- Reusing their TS/Node ecosystem plugins (lsp, e2b, extensions...) — those stay behind unless
  re-specified.

## OPTION 1 — Rust core host behind their web frontend
Keep: their `web` bundle UI. Rebuild: everything server-side in Rust speaking their Connection/API.
- Pros: instant polished UI; visible parity target; demo-able early.
- Cons: married to an undocumented generated protocol at rc stage; UI features pull server scope
  (goals, plans, todo, skills, session-query...) far beyond the core loop; pinned upstream commit.
- Effort shape: ~25–40K Rust lines incl. protocol layer; the risk lives in protocol drift.

## OPTION 2 — Clean Rust harness, "inspired-by not port-of" (RECOMMENDED)
Spec: their architecture/turn-flow/event docs. Own: our protocol (simple JSON-RPC/WS), our minimal
UI or reuse of our existing dashboards, compile-time composition.
- Pros: fully owned, no upstream coupling, smallest surface (15–25K lines), can cherry-pick their
  best ideas (append-only session log, event taxonomy, guarded tool pipeline, capability seams);
  license-clean (MIT; keep notices only for directly translated fragments).
- Cons: no ready-made frontend; feature scope is whatever we define (that is also the point).
- Phases: (1) session log + turn engine + one LLM adapter + shell/subprocess tools, headless CLI;
  (2) MCP client + sandbox/approval guard; (3) thin WS protocol + minimal web view; (4) compaction,
  subagents, skills as needed.

## Decision status
AWAITING CAPTAIN PICK: option 1 vs option 2 (or feasibility-only stop). Recommendation: option 2.
