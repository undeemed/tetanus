// Two suites, two coverage numbers, and they are never added together.
//
// `ours` is the adapter: the carrier, the fold, the store, the renderer table.
// Nobody upstream has ever run it, so it is held at 100% per file.
//
// `upstream` is DeepSeek Harness's own specs, vendored with the components
// they test. Upstream does NOT hold this code to 100% - their own
// `vitest.config.ts` excludes `ui-conversation/src/client/*`, `ui-tool/src/*`,
// `ui-slots/src/*` and most of `runtime/src` from their per-file gate, marked
// `TODO(gui)`. Adopting a number they do not hold would be inventing a claim;
// the number is measured and reported instead, and `NOTICE.md` and the report
// say which specs could not come across and why.
//
// Reporting them separately is the point. A single blended figure would let
// 8,000 well-covered vendored lines hide an uncovered branch in the 900 lines
// that are actually new here, which is the one place a defect can hide.

import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

const here = dirname(fileURLToPath(import.meta.url))
const at = (...parts: string[]): string => resolve(here, ...parts)

/** The same alias table the build uses; see `vite.config.ts` for why. */
const VENDORED: Record<string, string> = {
  '@deepseek-ai/cordis': at('upstream/cordis'),
  '@deepseek-ai/cosmokit': at('upstream/cosmokit'),
  '@deepseek-ai/dsh-attachment': at('upstream/attachment'),
  '@deepseek-ai/dsh-client-runtime': at('upstream/runtime'),
  '@deepseek-ai/dsh-client-ui-attachment': at('upstream/ui-attachment'),
  '@deepseek-ai/dsh-client-ui-conversation': at('upstream/ui-conversation'),
  '@deepseek-ai/dsh-client-ui-primitives': at('upstream/ui-primitives'),
  '@deepseek-ai/dsh-client-ui-slots': at('upstream/ui-slots'),
  '@deepseek-ai/dsh-client-ui-theme': at('upstream/ui-theme'),
  '@deepseek-ai/dsh-client-ui-tool': at('upstream/ui-tool'),
  '@deepseek-ai/dsh-host-apiproxy': at('upstream/apiproxy'),
  '@deepseek-ai/dsh-llm': at('upstream/llm'),
  '@deepseek-ai/dsh-session': at('upstream/session'),
}

const alias = Object.entries(VENDORED).map(([name, dir]) => ({
  find: new RegExp(`^${name.replace(/[/\-]/g, (c) => `\\${c}`)}(/.*)?$`),
  replacement: `${dir}$1`,
}))

/**
 * Which lane is running.
 *
 * `ours` and `upstream` are the two gated lanes. `vendored` is a measurement
 * rather than a gate: it runs BOTH suites and measures the vendored tree,
 * because our own specs drive upstream's components hard - `app.spec.tsx`
 * pushes a whole turn through `ChatView`, the node views and the tool tree -
 * and a number that counted only upstream's own suites would report those
 * packages at 0% while they are being exercised on every run.
 */
const lane = process.env['PANEL_SUITE'] ?? 'ours'
const half = lane === 'ours' ? 'ours' : 'upstream'

export default defineConfig({
  plugins: [react()],
  resolve: { alias, extensions: ['.ts', '.tsx', '.js', '.jsx', '.json'] },
  css: { modules: { generateScopedName: '[name]__[local]__[hash:base64:5]' } },
  test: {
    // Node by default, and jsdom per file through the `@vitest-environment`
    // pragma upstream's component specs already carry. Forcing jsdom globally
    // instead breaks the specs that read a stylesheet off disk: jsdom rewrites
    // `import.meta.url` to an http URL and `fileURLToPath` refuses it.
    environment: 'node',
    globals: false,
    // Shiki loads a grammar per language on demand, and a cold load outruns
    // the 5s default. Two of upstream's suites - the grammar loader and the
    // per-prefix incremental markdown comparison - are genuinely long rather
    // than racy: they do more work, so a bigger threshold strengthens them
    // instead of hiding anything. This box runs several build lanes at once
    // and they took 36s there; 180s is the margin, not the expectation.
    testTimeout: half === 'upstream' ? 180_000 : 30_000,
    include: lane === 'vendored'
      ? ['tests/**/*.spec.{ts,tsx}', 'upstream/**/tests/**/*.spec.{ts,tsx}']
      : half === 'upstream'
        ? ['upstream/**/tests/**/*.spec.{ts,tsx}']
        : ['tests/**/*.spec.{ts,tsx}'],
    // The engine case dials a real \`tetanus serve\` and is started by
    // \`crates/host/tests/panel_engine.rs\`, which passes the address. It is out
    // of the unit lane because it needs a server and because its coverage
    // would be measured against a peer nobody stubbed.
    // It self-skips without an address; the Rust gate is what stops that skip
    // from being how it always runs.
    // The engine case dials a real server and is started by the Rust gate,
    // which sets the address. Without one it self-skips rather than failing on
    // a connection nobody offered.
    exclude: lane === 'vendored'
      // A performance bound, not a behaviour assertion: upstream requires a
      // pathological markdown input to bail in under 3s, and v8's coverage
      // instrumentation alone puts it past that on an idle machine. It runs,
      // and passes, in the ungated `test:upstream` lane where nothing is
      // instrumented.
      ? ['node_modules/**', 'dist/**', 'upstream/ui-primitives/tests/markdown.client.spec.tsx']
      : ['node_modules/**', 'dist/**'],
    coverage: {
      provider: 'v8',
      enabled: false,
      include: half === 'upstream'
        ? ['upstream/**/*.{ts,tsx}']
        : ['src/**/*.{ts,tsx}'],
      exclude: half === 'upstream'
        ? ['upstream/**/tests/**']
        : [
            // Product strings. Four exported constants and no branch; a test
            // asserting a constant equals itself is decoration.
            'src/brand.ts',
          ],
      reporter: ['text', 'json-summary'],
      // Per file, so a well-covered big file cannot subsidise a bare one -
      // upstream's own reasoning, kept. Only `ours` is gated; see the header.
      thresholds: half === 'upstream'
        ? undefined
        : { perFile: true, statements: 100, branches: 100, functions: 100, lines: 100 },
    },
  },
})
