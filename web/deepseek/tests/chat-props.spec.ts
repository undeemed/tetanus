// @vitest-environment jsdom
//
// The seats handed to upstream's `ChatView`.
//
// Written out rather than spread from a context, because the list IS the
// coupling to upstream - and half of them are seats this screen honours and
// leaves empty. An empty seat is still a contract: `chatScroll.read` answering
// `undefined` instead of `null` is a crash inside upstream's layout effect,
// and no click reaches it.

import { describe, expect, it } from 'vitest'
import { chatProps } from '../src/App.tsx'
import { usePanel } from '../src/store.ts'

const props = () => chatProps('s1') as Record<string, (...args: never[]) => unknown>

describe('the stores', () => {
  it('useSession is the panel store itself, so a selector subscribes', () => {
    expect(props()['useSession']).toBe(usePanel)
  })

  it('useSessions answers the workspace root for this session and no other', () => {
    const select = props()['useSessions'] as (fn: (state: never) => unknown) => unknown
    expect(select((state: never) => (state as { byId: Record<string, unknown> }).byId['s1'])).toEqual({ cwd: undefined })
    expect(select((state: never) => (state as { byId: Record<string, unknown> }).byId['other'])).toBeUndefined()
  })

  it('useStore answers no details selection, because no screen here opens one', () => {
    const select = props()['useStore'] as (fn: (state: never) => unknown) => unknown
    expect(select((state: never) => (state as { selection: unknown }).selection)).toBeUndefined()
  })
})

describe('the seats this screen honours and leaves empty', () => {
  it('opening a file, inspecting a call and forking do nothing and throw nothing', () => {
    const seats = props()
    for (const name of ['openFile', 'inspectCall', 'forkAt', 'loadOlder']) {
      expect(() => (seats[name] as (arg?: unknown) => void)('x'), name).not.toThrow()
      expect((seats[name] as (arg?: unknown) => unknown)('x'), name).toBeUndefined()
    }
  })

  it('loading an image is refused with the reason upstream will show', async () => {
    // Rejecting rather than hanging: upstream awaits this to draw an inline
    // image, and a promise that never settles is a spinner that never stops.
    await expect((props()['loadImage'] as () => Promise<string>)())
      .rejects.toThrow('Image loading service unavailable')
  })

  it('there are no file mentions to resolve', () => {
    expect((props()['fileMentions'] as () => unknown)()).toBeUndefined()
  })
})

describe('the scroll seat', () => {
  it('reads null - not undefined - which is what upstream tests for', () => {
    const scroll = props()['chatScroll'] as unknown as { read: () => unknown; save: (at: unknown) => void }
    expect(scroll.read()).toBeNull()
  })

  it('accepts a saved position and forgets it, every time', () => {
    const scroll = props()['chatScroll'] as unknown as { read: () => unknown; save: (at: unknown) => void }
    expect(() => { scroll.save({ anchorKey: 'k', anchorTop: 1, scrollTop: 2 }) }).not.toThrow()
    expect(() => { scroll.save(null) }).not.toThrow()
    expect(scroll.read()).toBeNull()
  })
})

describe('the rest of the share', () => {
  it('carries the session id and the two functions upstream dispatches through', () => {
    const seats = props()
    expect(seats['sessionId']).toBe('s1')
    expect(typeof seats['renderSlot']).toBe('function')
    expect(typeof seats['t']).toBe('function')
  })
})
