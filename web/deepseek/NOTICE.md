# Third-party notices for the browser panel

This directory contains software from another project.
`upstream/` is source code from **DeepSeek Harness** (`deepseek-ai/deepseek-harness`), copied under the MIT licence.
Everything outside `upstream/` was written for tetanus and carries tetanus's own licence, `MIT OR Apache-2.0`.

## What was copied

The conversation view and what it needs to run, plus upstream's own specs for the packages that came across: **178 source files and 26 spec files** across twelve of upstream's packages, listed below by the directory each was vendored into.
Each file keeps a header naming the exact upstream path it came from.
`tools/vendor.py` recomputes the set from a checkout, so the list is a fact about upstream's source rather than a list anybody maintains by hand.

| Vendored as | Upstream package |
| --- | --- |
| `upstream/ui-conversation` | `@deepseek-ai/dsh-client-ui-conversation` |
| `upstream/ui-tool` | `@deepseek-ai/dsh-client-ui-tool` |
| `upstream/ui-primitives` | `@deepseek-ai/dsh-client-ui-primitives` |
| `upstream/ui-attachment` | `@deepseek-ai/dsh-client-ui-attachment` |
| `upstream/ui-slots` | `@deepseek-ai/dsh-client-ui-slots` |
| `upstream/ui-theme` | `@deepseek-ai/dsh-client-ui-theme` |
| `upstream/runtime` | `@deepseek-ai/dsh-client-runtime` |
| `upstream/apiproxy`, `upstream/llm`, `upstream/session` | `@deepseek-ai/dsh-host-apiproxy`, `@deepseek-ai/dsh-llm`, `@deepseek-ai/dsh-session` |
| `upstream/cordis`, `upstream/cosmokit` | `@deepseek-ai/cordis`, `@deepseek-ai/cosmokit` |
| `upstream/attachment` | `@deepseek-ai/dsh-attachment` |

Upstream's specs live in `upstream/<package>/tests/`, beside the code they test, with their fixture goldens.
43 of their 69 could not come across; `upstream/SPECS-NOT-PORTED.txt` names every one and why, and section 7.3 of `data/tetanus-ui-port/report.md` explains the two reasons.

## The licence

Upstream's `LICENSE` is reproduced verbatim at `upstream/LICENSE`.
It is the MIT licence, `Copyright (c) 2026 DeepSeek`, and it is the licence declared by the root `LICENSE` of the checkout these files came from and by all 221 of that checkout's `package.json` files.

MIT requires the copyright notice and the permission notice to travel with substantial portions of the software.
Three things carry them, and `crates/host/tests/panel_port.rs::every_vendored_file_carries_its_notice` asserts all three:

1. `upstream/LICENSE`, the full text, beside the code it licenses. It also covers the snapshot goldens under `upstream/ui-primitives/tests/fixtures/`, which are data files with nowhere to put a comment - a header inside a golden *is* the golden.
2. A header on every single vendored file naming the copyright holder, the licence, and the upstream path.
3. A different header on a file this project modified, saying so - because a copy that was edited must not go on claiming it was not.

### One thing that is deliberately not copied

DeepSeek's whale mark and the `deepseek-official HARNESS` letterforms are **trade marks**, and `upstream/ui-primitives/BrandWordmark.tsx` and `FishLogo.tsx` are those marks drawn as SVG.
A copyright licence grants copyright permission and says nothing about trade marks, so shipping that art inside a product called something else is the one thing the MIT grant does not cover.
`tools/vendor.py` refuses both files and the primitives barrel drops their re-exports.
Nothing in the conversation view referenced either symbol.
`panel_port.rs::upstream_brand_art_is_not_vendored` keeps it that way.

### The published packages are under a different licence

Worth recording, because it is a trap for whoever refreshes this next.
The same packages **published to npm** at `0.0.1-rc.1` ship a **BSD-3-Clause** `LICENSE` and declare `"license": "BSD-3-Clause"`, not MIT.
Everything here comes from the source checkout, which is MIT, so MIT is the licence that applies to these copies.
Do not mix the two sources: an npm tarball dropped into `upstream/` would bring BSD-3-Clause obligations - including its clause 3, which forbids using the copyright holder's name to promote a derived product - under headers claiming MIT.

## What is ours, and where the line is

- `src/` - the carrier, the journal fold, the store, the renderer table, the locale seat, the chrome. Ours.
- `upstream/` - theirs, unmodified except for the two files below.
- `tools/vendor.py`, `vite.config.ts`, `index.html`, `tsconfig.json`, `types/` - ours.

Two vendored files carry an edit, each stating it in its own header:

| File | What changed | Why |
| --- | --- | --- |
| `upstream/ui-conversation/client/chat/ChatView.tsx` | the running-turn label, `Deep diving...` to `Thinking...` | product voice, not attribution |
| `upstream/ui-primitives/index.ts` | two brand-art re-exports removed | trade marks, see above |
| `upstream/ui-primitives/tests/icons.client.spec.tsx` | the `FishLogo` suite removed | it tests art that is not vendored |
| several vendored specs | per-case `{ timeout: N }` raised to 180s | this build box is shared; upstream's is not. No assertion changed |
| `upstream/ui-primitives/tests/fixtures/*` | snapshot goldens re-baselined | a CSS-module hash is derived from the file path, which changed. Proven hash-only in report section 7.4 |

Every vendored spec states in its own header which of these applies to it.
