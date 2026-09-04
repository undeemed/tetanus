# The browser panel, ported from DeepSeek Harness

DeepSeek Harness's conversation view, running against tetanus's engine over the JSON-RPC carrier this repository already serves.

This is **not** the panel that ships today.
`web/app` is, and it is untouched: it needs no build step and it is what `tetanus serve --frontend web/app` serves.
This directory is the replacement candidate, and it is kept beside the working one until somebody has looked at both.

## Running it

```sh
cd web/deepseek && pnpm install && pnpm run build
cargo run --bin tetanus -- serve --listen 127.0.0.1:5300 --frontend web/deepseek/dist
```

Then open `http://127.0.0.1:5300`.
The page reads `window.TETANUS_BOOT`, which `crates/host` writes in through its index tap, so it dials the carrier it was told about rather than one it guessed at.

`?ws=<url>` overrides the carrier, and `?token=<secret>` supplies one for a deployment that needs it (contract §4.1.2).

## What is here

| Path | What it is |
| --- | --- |
| `upstream/` | DeepSeek Harness source, vendored. See `NOTICE.md` before touching it. |
| `src/carrier.ts` | JSON-RPC 2.0 over the WebSocket at `/api/ws`. |
| `src/timeline.ts` | **The port.** Our journal folded into the node model upstream's view reads. |
| `src/store.ts` | The three values `ChatView` selects from. |
| `src/renderers.tsx` | Upstream's keyed node table, without the plugin container that usually fills it. |
| `src/App.tsx` | One session, a composer, and the view. |
| `tools/vendor.py` | Recomputes `upstream/` from a Harness checkout. |

## Refreshing `upstream/`

```sh
python3 tools/vendor.py /path/to/deepseek-harness
```

It reads the checkout and never writes to it.
Files this project modified on purpose are listed in the script and are left alone, so an edit is never silently reverted.
Run the build afterwards: the closure is computed from what the entry points import, so an upstream refactor changes the file set and the build is what says whether it still links.

## Why it is built this way

The long answer is `data/tetanus-ui-port/report.md`.
The short one:

- Upstream's published npm packages **cannot be installed** - `@deepseek-ai/dsh-client-runtime` depends on `@deepseek-ai/dsh-compact` and `@deepseek-ai/dsh-host-apiproxy` on `@deepseek-ai/dsh-user-interaction`, and neither exists on the registry. So the source is vendored rather than depended on.
- Their conversation view's **runtime** closure is 175 files. The other 40,000 lines a naive trace finds are type-only imports the transpiler erases.
- The gap between the two projects is not the HTTP paths, it is the **derived** conversation model. `src/timeline.ts` is that gap, written out.

## The build is a test

`crates/host/tests/panel_port.rs` runs `pnpm run build` and `pnpm run check`, so `cargo test --workspace` - this project's merge gate - fails when the panel does not build.
On a machine with no Node the case says loudly what it did not check and passes.
In CI it fails, because a skip in CI is not protection.
