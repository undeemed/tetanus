# DeepSeek Harness → Rust rewrite: scoping note

> **Superseded.** This is the first-pass scoping note, kept for provenance only.
> The decision document is [PLAN.md](PLAN.md); the built system is [../ARCHITECTURE.md](../ARCHITECTURE.md).
> Nothing here is maintained, and the option list below was settled in favour of option 2 at
> full-parity scope on 2026-08-18.

Captain asked (2026-08-18): "<https://github.com/deepseek-ai/deepseek-harness> can i rewrite this in rust?"

## Facts

- Repo: deepseek-ai/deepseek-harness, MIT (c) 2026 DeepSeek. Version 0.1.0-rc.7, developer preview, breaking changes expected.
- Local clone: /tmp/deepseek-harness (81M, shallow). NOTE: /tmp is volatile — re-clone if missing.
- Total: ~568K lines of TS across ~50 workspace packages + web frontend + Python SDK + native/landlock-run sandbox helper.
- Plugin architecture on Cordis (vendor/cordis, schemastery, hmr) — TS-runtime-specific; does NOT map 1:1 to Rust.

## Core-loop LOC inventory (packages/, TS lines)

- core: 40,851
- host: 22,387
- session: 21,442
- llm: 20,792
- shell: 10,408
- sandbox: 9,040
- terminal: 5,745
- api: 4,415
- subprocess: 4,256
- mcp: 4,016
→ core harness surface ≈ 143K TS lines; a Rust reimplementation of the essential loop (agent loop, session/state, MCP client, sandbox exec, HTTP/WS protocol host) estimated 15–25K Rust lines.

## Legal

- MIT: rewrite/redistribute/sell fine; retain license + copyright notice for directly translated portions; honor THIRD_PARTY_NOTICES.md for vendored deps. No CLA needed (fork, not upstream).

## Options (recommended order)

1. Rust core host speaking their existing web-UI protocol; keep their web frontend. Feasible, bounded.
2. Clean Rust harness using docs/architecture.md as spec ("inspired-by, not port-of"). Fastest to something owned.
3. Full 1:1 port — advise against (Cordis paradigm, Node ecosystem deps, rc-stage moving target).

## Key references in clone

- docs/architecture.md, docs/development.md, AGENTS.md
- packages/api/src (protocol surface for option 1)
