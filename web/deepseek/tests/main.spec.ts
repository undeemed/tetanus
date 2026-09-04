// @vitest-environment jsdom
//
// The entry point. Five lines, and one of them is the difference between a
// visible failure and a blank page: a `#root` that is not there.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const render = vi.hoisted(() => vi.fn())

// The whole app is stubbed out. This file is about the mount, and importing
// the real tree would make it about the app instead.
vi.mock('react-dom/client', () => ({ createRoot: () => ({ render }) }))
vi.mock('../src/App.tsx', () => ({ App: () => null }))
vi.mock('@deepseek-ai/dsh-client-ui-theme/styles/base.css', () => ({}))
vi.mock('@deepseek-ai/dsh-client-ui-theme/styles/design-platform.css', () => ({}))
vi.mock('@deepseek-ai/dsh-client-ui-theme/styles/scrollbar.css', () => ({}))
vi.mock('@deepseek-ai/dsh-client-ui-theme/styles/shiki.css', () => ({}))

beforeEach(() => {
  render.mockClear()
  vi.resetModules()
  document.body.innerHTML = ''
  document.title = ''
})

afterEach(() => { document.body.innerHTML = '' })

describe('the entry point', () => {
  it('sets the product title and mounts into #root', async () => {
    document.body.innerHTML = '<div id="root"></div>'
    await import('../src/main.tsx')
    expect(document.title).toBe('tetanus panel')
    expect(render).toHaveBeenCalledOnce()
  })

  it('throws a sentence rather than mounting into nothing', async () => {
    // The alternative is `createRoot(null)`, whose own error names neither the
    // page nor the id - a blank screen and a stack trace about React.
    await expect(import('../src/main.tsx')).rejects.toThrow('the page has no #root to mount into')
  })
})
