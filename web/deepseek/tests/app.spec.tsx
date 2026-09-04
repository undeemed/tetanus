// @vitest-environment jsdom
//
// The screen, driven end to end against a socket this file plays the engine on.
//
// This is the integration case for the panel: it does not poke the store, it
// types into the composer, answers the frames a real engine would answer, and
// reads the DOM upstream's components produced. Everything between - the
// handshake ordering, the session/subscribe/page sequence, the fold, the
// renderer table, the locale seat - is exercised by being needed.
//
// The seam it does NOT test is the wire itself, because the peer here agrees
// with us by construction. That is what `crates/host/tests/panel_engine.rs`
// is for: the same journey against the real binary.

import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { App } from '../src/App.tsx'
import { usePanel } from '../src/store.ts'

interface Frame {
  id?: number
  method?: string
  params?: Record<string, unknown>
}

/** The engine, as far as the page can tell. */
class Engine {
  static readonly CONNECTING = 0
  static readonly OPEN = 1
  static readonly CLOSING = 2
  static readonly CLOSED = 3
  static last: Engine | undefined

  readyState = Engine.CONNECTING
  closed = false
  readonly asked: Frame[] = []
  /** Methods this engine should answer with an error instead of a result. */
  static refuse = new Set<string>()
  /** What `session.events` answers with. Empty is a fresh session. */
  static page: unknown[] = []
  /** Results that replace the default answer, by method. */
  static answer: Record<string, unknown> = {}
  /** Errors that replace the default answer, by method. */
  static answerError: Record<string, unknown> = {}
  private readonly listeners = new Map<string, Array<(event: unknown) => void>>()

  constructor(readonly url: string | URL) {
    Engine.last = this
    queueMicrotask(() => {
      this.readyState = Engine.OPEN
      this.fire('open')
    })
  }

  addEventListener(name: string, handler: (event: unknown) => void): void {
    const found = this.listeners.get(name) ?? []
    found.push(handler)
    this.listeners.set(name, found)
  }

  close(): void {
    this.closed = true
    this.readyState = Engine.CLOSED
  }

  fire(name: string, event: unknown = {}): void {
    for (const handler of this.listeners.get(name) ?? []) handler(event)
  }

  /** Answer a request the way the engine would. */
  send(text: string): void {
    const frame = JSON.parse(text) as Frame & { method: string; id: number }
    this.asked.push(frame)
    if (Engine.refuse.has(frame.method)) {
      this.reply(frame.id, undefined, { code: -32603, message: `${frame.method} refused` })
      return
    }
    if (frame.method in Engine.answerError) {
      this.reply(frame.id, undefined, Engine.answerError[frame.method])
      return
    }
    if (frame.method in Engine.answer) {
      this.reply(frame.id, Engine.answer[frame.method])
      return
    }
    switch (frame.method) {
      case 'rpc.hello':
        this.reply(frame.id, { protocol_version: '1.0', server: { name: 'tetanus', version: '0' }, capabilities: [] })
        return
      case 'session.create':
        this.reply(frame.id, { session_id: 's1', model: 'mock-echo-1', provider: 'mock', path: '/tmp/s1', last_seq: -1 })
        return
      case 'session.subscribe':
        this.reply(frame.id, {})
        return
      case 'session.events':
        this.reply(frame.id, { events: Engine.page, eof: true })
        return
      case 'agent.prompt':
        // Answers when the turn closes; the transcript arrived meanwhile.
        this.reply(frame.id, { summary: { turn: 1, steps: 1, stop_reason: 'natural' } })
        return
      default:
        this.reply(frame.id, {})
    }
  }

  private reply(id: number, result?: unknown, error?: unknown): void {
    queueMicrotask(() => {
      this.deliver(error === undefined
        ? { jsonrpc: '2.0', id, result }
        : { jsonrpc: '2.0', id, error })
    })
  }

  deliver(frame: unknown): void {
    this.fire('message', { data: JSON.stringify(frame) })
  }

  /** Push one journal line, as `session/event` would. */
  push(type: string, seq: number, data: unknown): void {
    this.deliver({
      jsonrpc: '2.0',
      method: 'session/event',
      params: { session_id: 's1', event: { type, seq, time: 1_700_000_000_000 + seq, data } },
    })
  }

  status(state: string): void {
    this.deliver({ jsonrpc: '2.0', method: 'agent/status', params: { session_id: 's1', state } })
  }
}

const settle = async (): Promise<void> => {
  await act(async () => { await new Promise((resolve) => { setTimeout(resolve, 0) }) })
}

/** Render the app and wait for the session to open. */
const opened = async (): Promise<Engine> => {
  render(<App />)
  await settle()
  const engine = Engine.last as Engine
  await waitFor(() => { expect(usePanel.getState().openState).toBe('open') })
  return engine
}

const composer = (): HTMLTextAreaElement =>
  screen.getByPlaceholderText('Message the agent') as HTMLTextAreaElement

beforeEach(() => {
  Engine.last = undefined
  Engine.refuse = new Set()
  Engine.page = []
  Engine.answer = {}
  Engine.answerError = {}
  vi.stubGlobal('WebSocket', Engine)
  ;(window as { TETANUS_BOOT?: unknown }).TETANUS_BOOT = { carrier: 'ws://engine/api/ws', protocol: '1.0' }
  usePanel.setState({
    sessionId: '', events: [], queue: [], running: false, openState: 'idle', openError: null,
    hasMore: false, loadingOlder: false, health: { state: 'connecting' }, model: '',
    chat: { order: [], nodes: new Map(), timeline: { turnOrder: [], turns: new Map() } },
  })
})

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
  delete (window as { TETANUS_BOOT?: unknown }).TETANUS_BOOT
})

describe('opening', () => {
  it('greets, creates, subscribes and pages, in that order', async () => {
    // Subscribing to an id the engine has not minted is the one ordering that
    // cannot work, so the order is the assertion.
    const engine = await opened()
    expect(engine.asked.map((each) => each.method)).toEqual([
      'rpc.hello', 'session.create', 'session.subscribe', 'session.events',
    ])
  })

  it('shows the model the engine composed the session with', async () => {
    await opened()
    expect(screen.getByText('mock-echo-1')).toBeTruthy()
  })

  it('shows the product name and not upstream\u2019s', async () => {
    await opened()
    expect(screen.getByText('tetanus')).toBeTruthy()
    expect(document.body.textContent).not.toContain('DeepSeek')
  })

  it('reports the connection as connected', async () => {
    await opened()
    expect(screen.getByText('connected')).toBeTruthy()
  })

  it('asks for a bounded first page rather than the whole journal', async () => {
    // An unbounded page is a frame the carrier may refuse outright
    // (`methods::MAX_FRAME_BYTES`), so the window is stated.
    const engine = await opened()
    const page = engine.asked.find((each) => each.method === 'session.events')
    expect(page?.params?.['session_id']).toBe('s1')
    expect(page?.params?.['limit']).toBe(500)
  })

  it('renders a journal the first page already held', async () => {
    // Opening an existing session is the case `session.events` exists for; a
    // fresh one answering empty is the easy half.
    Engine.page = [
      { type: 'user/message', seq: 0, time: 1, data: { content: 'from the journal' } },
      { type: 'assistant/message', seq: 1, time: 2, data: { content: 'answered before' } },
    ]
    await opened()
    expect(screen.getByText('from the journal')).toBeTruthy()
    expect(screen.getByText('answered before')).toBeTruthy()
  })

  it('shows the engine\u2019s own error when the session cannot be created', async () => {
    Engine.refuse = new Set(['session.create'])
    render(<App />)
    await settle()
    await waitFor(() => { expect(usePanel.getState().openState).toBe('error') })
    expect(screen.getByText(/session.create refused/)).toBeTruthy()
  })

  it('reports a carrier that will not open at all', async () => {
    render(<App />)
    await act(async () => {
      ;(Engine.last as Engine).fire('error')
      await Promise.resolve()
    })
    await waitFor(() => { expect(usePanel.getState().openState).toBe('error') })
  })

  it('survives a session.create that answers without an id or a model', async () => {
    // The contract says both are present, and a surface that trusted that
    // would render `undefined` into the header and prompt against the string
    // "undefined". Neither is a failure worth blanking the screen for.
    Engine.answer = { 'session.create': {} }
    render(<App />)
    await settle()
    await waitFor(() => { expect(usePanel.getState().openState).toBe('open') })
    expect(usePanel.getState().sessionId).toBe('')
    expect(usePanel.getState().model).toBe('')
    // No model means no pill, rather than an empty one.
    expect(document.querySelector('[data-state]')).toBeTruthy()
    expect(screen.queryByText('mock-echo-1')).toBeNull()
  })

  it('survives a first page whose events are not a list', async () => {
    Engine.answer = { 'session.events': { events: 'not a list', eof: true } }
    await opened()
    expect(usePanel.getState().events).toEqual([])
  })

  it('reports a failure that is not a contract error at all', async () => {
    // A rejection with neither a numeric code nor a string message still has
    // to reach the reader as something: the fallbacks are what stop the panel
    // showing `undefined` and no code.
    Engine.answerError = { 'session.create': {} }
    render(<App />)
    await settle()
    await waitFor(() => { expect(usePanel.getState().openState).toBe('error') })
    expect(usePanel.getState().openError?.code).toBe(-32000)
    expect(usePanel.getState().openError?.message).toBe('[object Object]')
  })

  it('a screen unmounted mid-handshake never writes to the store', async () => {
    const { unmount } = render(<App />)
    unmount()
    await settle()
    expect(usePanel.getState().openState).toBe('loading')
    expect(usePanel.getState().sessionId).toBe('')
  })

  it('closes the socket when the screen goes away', async () => {
    const { unmount } = render(<App />)
    await settle()
    const engine = Engine.last as Engine
    unmount()
    expect(engine.closed).toBe(true)
  })
})

describe('a whole turn', () => {
  it('draws the question, the streamed answer, the tool card and the settled answer', async () => {
    const engine = await opened()

    fireEvent.change(composer(), { target: { value: '! echo hello' } })
    await act(async () => { fireEvent.click(screen.getByText('Send message')) })

    await act(async () => {
      engine.status('running')
      engine.push('turn/start', 0, { turn: 1 })
      engine.push('user/message', 1, { content: '! echo hello' })
      engine.push('step/start', 2, { turn: 1, step: 1 })
      engine.push('assistant/chunk', 3, { chunk: 'text', delta: 'Let me ' })
      engine.push('assistant/chunk', 4, { chunk: 'text', delta: 'run that.' })
      engine.push('tool/call', 5, { id: 'c1', name: 'shell', arguments: '{"command":"echo hello"}' })
      await Promise.resolve()
    })

    expect(screen.getByText('! echo hello')).toBeTruthy()
    expect(screen.getByText('Let me run that.')).toBeTruthy()
    expect(document.querySelector('[data-chat-call-id="c1"]')).toBeTruthy()

    await act(async () => {
      engine.push('tool/result', 6, { call_id: 'c1', name: 'shell', ok: true, content: 'hello' })
      engine.push('assistant/message', 7, { content: 'It printed hello.', reasoning: '', tool_calls: [] })
      engine.push('step/end', 8, { turn: 1, step: 1 })
      engine.push('turn/end', 9, { turn: 1, steps: 1, stop_reason: 'natural' })
      engine.status('idle')
      await Promise.resolve()
    })

    expect(screen.getByText('It printed hello.')).toBeTruthy()
    expect(document.querySelector('[data-state="ok"]')).toBeTruthy()
    // The streamed text was replaced, not duplicated below the tool card.
    expect(screen.queryByText('Let me run that.')).toBeNull()
  })

  it('shows the running label while a turn is open and drops it after', async () => {
    const engine = await opened()
    await act(async () => {
      engine.status('running')
      engine.push('turn/start', 0, { turn: 1 })
      await Promise.resolve()
    })
    expect(screen.getByText(/Thinking/)).toBeTruthy()
    await act(async () => {
      engine.push('turn/end', 1, { turn: 1 })
      engine.status('idle')
      await Promise.resolve()
    })
    expect(screen.queryByText(/Thinking/)).toBeNull()
  })

  it('draws an event this build has no shaped row for, rather than dropping it', async () => {
    const engine = await opened()
    await act(async () => {
      engine.push('request/context', 0, { tokens: 12 })
      await Promise.resolve()
    })
    expect(screen.getByText(/request\/context/)).toBeTruthy()
  })
})

describe('the composer', () => {
  it('will not send an empty message', async () => {
    await opened()
    expect((screen.getByText('Send message') as HTMLButtonElement).disabled).toBe(true)
  })

  it('will not send whitespace either', async () => {
    const engine = await opened()
    fireEvent.change(composer(), { target: { value: '   ' } })
    expect((screen.getByText('Send message') as HTMLButtonElement).disabled).toBe(true)
    await act(async () => { fireEvent.click(screen.getByText('Send message')) })
    expect(engine.asked.some((each) => each.method === 'agent.prompt')).toBe(false)
  })

  it('trims what it sends', async () => {
    const engine = await opened()
    fireEvent.change(composer(), { target: { value: '  hello  ' } })
    await act(async () => { fireEvent.click(screen.getByText('Send message')) })
    const prompt = engine.asked.find((each) => each.method === 'agent.prompt')
    expect(prompt?.params?.['content']).toBe('hello')
  })

  it('clears itself on send, so a double click does not send twice', async () => {
    await opened()
    fireEvent.change(composer(), { target: { value: 'once' } })
    await act(async () => { fireEvent.click(screen.getByText('Send message')) })
    expect(composer().value).toBe('')
  })

  it('sends on Enter', async () => {
    const engine = await opened()
    fireEvent.change(composer(), { target: { value: 'by key' } })
    await act(async () => { fireEvent.keyDown(composer(), { key: 'Enter' }) })
    expect(engine.asked.some((each) => each.method === 'agent.prompt')).toBe(true)
  })

  it('does not send on Shift+Enter, which is a newline', async () => {
    const engine = await opened()
    fireEvent.change(composer(), { target: { value: 'multi' } })
    await act(async () => { fireEvent.keyDown(composer(), { key: 'Enter', shiftKey: true }) })
    expect(engine.asked.some((each) => each.method === 'agent.prompt')).toBe(false)
  })

  it('ignores other keys', async () => {
    const engine = await opened()
    fireEvent.change(composer(), { target: { value: 'x' } })
    await act(async () => { fireEvent.keyDown(composer(), { key: 'a' }) })
    expect(engine.asked.some((each) => each.method === 'agent.prompt')).toBe(false)
  })

  it('reports a prompt the engine refused', async () => {
    Engine.refuse = new Set(['agent.prompt'])
    const engine = await opened()
    fireEvent.change(composer(), { target: { value: 'go' } })
    await act(async () => { fireEvent.click(screen.getByText('Send message')) })
    await settle()
    expect(engine.asked.some((each) => each.method === 'agent.prompt')).toBe(true)
    await waitFor(() => { expect(usePanel.getState().openState).toBe('error') })
  })

  it('will not prompt before a session exists', async () => {
    render(<App />)
    // Deliberately before the handshake settles.
    fireEvent.change(composer(), { target: { value: 'too early' } })
    await act(async () => { fireEvent.click(screen.getByText('Send message')) })
    expect((Engine.last as Engine).asked.some((each) => each.method === 'agent.prompt')).toBe(false)
    await settle()
  })
})

describe('stopping', () => {
  it('offers Stop while a turn runs, and interrupts the session', async () => {
    const engine = await opened()
    await act(async () => {
      engine.status('running')
      await Promise.resolve()
    })
    await act(async () => { fireEvent.click(screen.getByText('Stop generating')) })
    const stopped = engine.asked.find((each) => each.method === 'agent.interrupt')
    expect(stopped?.params?.['session_id']).toBe('s1')
  })

  it('swallows a refused interrupt rather than blanking the transcript', async () => {
    // A turn that has already ended answers `agent.interrupt` with an error,
    // and that is a race rather than a fault the reader can act on.
    Engine.refuse = new Set(['agent.interrupt'])
    const engine = await opened()
    await act(async () => {
      engine.status('running')
      await Promise.resolve()
    })
    await act(async () => { fireEvent.click(screen.getByText('Stop generating')) })
    await settle()
    expect(usePanel.getState().openState).toBe('open')
  })

  it('does nothing when there is no session to interrupt', async () => {
    const engine = await opened()
    await act(async () => { usePanel.setState({ sessionId: '', running: true }) })
    await act(async () => { fireEvent.click(screen.getByText('Stop generating')) })
    expect(engine.asked.some((each) => each.method === 'agent.interrupt')).toBe(false)
  })
})

describe('the connection pill', () => {
  it('says what went wrong when the socket closes under the page', async () => {
    const engine = await opened()
    await act(async () => {
      engine.fire('close', { code: 1011, reason: 'engine went away' })
      await Promise.resolve()
    })
    expect(screen.getByText('closed')).toBeTruthy()
  })
})
