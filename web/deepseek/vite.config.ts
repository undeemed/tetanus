// The panel's build.
//
// `upstream/` is DeepSeek Harness source, vendored verbatim (see
// `upstream/LICENSE` and `NOTICE.md`). It was written for a pnpm workspace, so
// every cross-package import it carries spells a package name that does not
// exist here. Rather than rewrite 167 vendored files - which would make the
// next upstream refresh a merge instead of a copy - the package names are
// resolved to the vendored directories by one alias table.
//
// The table is the whole of the coupling to upstream's layout, which is why it
// is written out rather than derived: a name that stops resolving fails the
// build loudly, and a name added by a refresh is one line here.

import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

const here = dirname(fileURLToPath(import.meta.url))
const at = (...parts: string[]): string => resolve(here, ...parts)

/** Upstream package name -> the directory it was vendored into. */
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

const aliases = Object.entries(VENDORED).map(([name, dir]) => ({
  find: new RegExp(`^${name.replace(/[/\-]/g, (c) => `\\${c}`)}(/.*)?$`),
  replacement: `${dir}$1`,
}))

export default defineConfig({
  base: './',
  plugins: [react()],
  resolve: {
    alias: aliases,
    // Upstream writes explicit `.ts`/`.tsx` extensions on its relative
    // imports, which vite resolves natively; nothing here needs an
    // extension guess.
    extensions: ['.ts', '.tsx', '.js', '.jsx', '.json'],
  },
  css: {
    modules: {
      // Upstream's CSS modules are keyed by their exported name, and the
      // hashed part keeps two files that both export `.row` apart. This is
      // the mechanism TC-WEB-1 and TC-WEB-2 were written to check by hand.
      generateScopedName: '[name]__[local]__[hash:base64:5]',
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    // A source map per chunk is most of the built size and nothing serves
    // it; a stack trace worth reading is reproduced with `pnpm dev`.
    sourcemap: false,
    chunkSizeWarningLimit: 2048,
  },
})
