// The store `ChatView` selects from.
//
// Small, and the two interesting cases are both about a stream that repeats
// itself: a subscription replays what the first page already carried, and a
// running flag that is folded into the wrong snapshot leaves the "Thinking"
// label stuck on a finished transcript.

import { beforeEach, describe, expect, it } from 'vitest'
import { usePanel } from '../src/store.ts'
import type { SessionEvent } from '../src/carrier.ts'

const event = (seq: number, type = 'user/message', data: unknown = { content: 'x' }): SessionEvent =>
  ({ type, seq, time: 1_700_000_000_000 + seq, data: data as Record<string, unknown> })

const state = () => usePanel.getState()

beforeEach(() => {
  usePanel.setState({
    sessionId: '',
    events: [],
    chat: { order: [], nodes: new Map(), timeline: { turnOrder: [], turns: new Map() } },
    queue: [],
    running: false,
    openState: 'idle',
    openError: null,
    hasMore: false,
    loadingOlder: false,
    health: { state: 'connecting' },
    model: '',
  })
})

describe('opened', () => {
  it('takes the session, the model and the first window, and folds them', () => {
    state().opened('s1', 'deepseek-chat', [event(0)])
    expect(state().sessionId).toBe('s1')
    expect(state().model).toBe('deepseek-chat')
    expect(state().openState).toBe('open')
    expect(state().chat.order).toEqual(['user:0'])
  })

  it('clears an error from a previous attempt', () => {
    state().failed({ code: -1, message: 'first try' })
    state().opened('s1', 'm', [])
    expect(state().openError).toBeNull()
    expect(state().openState).toBe('open')
  })

  it('an empty window is the normal case for a fresh session', () => {
    state().opened('s1', 'm', [])
    expect(state().openState).toBe('open')
    expect(state().chat.order).toEqual([])
  })
})

describe('failed', () => {
  it('records the code and the message the engine gave', () => {
    state().failed({ code: -32001, message: 'not implemented' })
    expect(state().openState).toBe('error')
    expect(state().openError).toEqual({ code: -32001, message: 'not implemented' })
  })
})

describe('arrived', () => {
  it('appends and refolds', () => {
    state().opened('s1', 'm', [])
    state().arrived(event(0))
    expect(state().chat.order).toEqual(['user:0'])
  })

  it('drops a seq the window already holds', () => {
    // A reconnect replays. Without this the transcript draws every settled
    // answer twice, which reads as the model repeating itself.
    state().opened('s1', 'm', [event(0)])
    state().arrived(event(0))
    expect(state().events).toHaveLength(1)
  })

  it('keeps a genuinely new seq that arrives out of order', () => {
    state().opened('s1', 'm', [event(2)])
    state().arrived(event(1))
    expect(state().events.map((each) => each.seq)).toEqual([2, 1])
  })

  it('folds the arrival against the running flag currently held', () => {
    usePanel.setState({ running: true })
    state().arrived(event(0, 'turn/start', { turn: 1 }))
    expect(state().chat.timeline.turns.get(1)?.status).toBe('open')
  })
})

describe('ran', () => {
  it('refolds, so a finished turn stops being open the moment the agent idles', () => {
    state().opened('s1', 'm', [event(0, 'turn/start', { turn: 1 })])
    state().ran(true)
    expect(state().chat.timeline.turns.get(1)?.status).toBe('open')
    state().ran(false)
    expect(state().chat.timeline.turns.get(1)?.status).toBe('closed')
    expect(state().running).toBe(false)
  })
})

describe('healthChanged', () => {
  it('records the health without touching the transcript', () => {
    state().opened('s1', 'm', [event(0)])
    state().healthChanged({ state: 'closed', why: 'gone' })
    expect(state().health).toEqual({ state: 'closed', why: 'gone' })
    expect(state().chat.order).toEqual(['user:0'])
  })
})
