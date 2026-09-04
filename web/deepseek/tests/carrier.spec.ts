// @vitest-environment jsdom
//
// The carrier: JSON-RPC 2.0 over the socket `crates/rpc` serves.
//
// Everything hostile a socket can deliver is exercised here, because a browser
// carrier is the one component whose peer is not under our control even in
// principle: a frame that is not JSON, a frame that is JSON but not a message,
// a reply to a call nobody made, a binary frame, a close in the middle of a
// call. The rule is that none of them throws and none of them leaves a caller
// waiting for ever - which is the one failure a codec must not have.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { Carrier } from '../src/carrier.ts'
import type { Health, SessionEvent } from '../src/carrier.ts'

/** A socket this test drives directly. */
class FakeSocket {
  static readonly CONNECTING = 0
  static readonly OPEN = 1
  static readonly CLOSING = 2
  static readonly CLOSED = 3

  static last: FakeSocket | undefined
  readonly sent: string[] = []
  readyState = FakeSocket.CONNECTING
  closed = false
  private readonly listeners = new Map<string, Array<(event: unknown) => void>>()

  constructor(readonly url: string | URL) {
    FakeSocket.last = this
  }

  addEventListener(name: string, handler: (event: unknown) => void): void {
    const found = this.listeners.get(name) ?? []
    found.push(handler)
    this.listeners.set(name, found)
  }

  send(text: string): void {
    this.sent.push(text)
  }

  close(): void {
    this.closed = true
    this.readyState = FakeSocket.CLOSED
  }

  /** Drive the socket the way a peer would. */
  fire(name: string, event: unknown = {}): void {
    if (name === 'open') this.readyState = FakeSocket.OPEN
    for (const handler of this.listeners.get(name) ?? []) handler(event)
  }

  deliver(frame: unknown): void {
    this.fire('message', { data: typeof frame === 'string' ? frame : JSON.stringify(frame) })
  }

  /** The id of the nth frame this socket was asked to send. */
  idOf(nth: number): number {
    return (JSON.parse(this.sent[nth] ?? '{}') as { id: number }).id
  }

  methodOf(nth: number): string {
    return (JSON.parse(this.sent[nth] ?? '{}') as { method: string }).method
  }
}

interface Seen {
  events: Array<[string, SessionEvent]>
  status: Array<[string, boolean]>
  health: Health[]
}

const watchers = (): [Seen, ConstructorParameters<typeof Carrier>] => {
  const seen: Seen = { events: [], status: [], health: [] }
  return [seen, [
    (session, event) => { seen.events.push([session, event]) },
    (session, running) => { seen.status.push([session, running]) },
    (health) => { seen.health.push(health) },
  ]]
}

const at = (search: string, boot?: Record<string, unknown>): void => {
  window.history.replaceState({}, '', `/${search}`)
  if (boot === undefined) delete (window as { TETANUS_BOOT?: unknown }).TETANUS_BOOT
  else (window as { TETANUS_BOOT?: unknown }).TETANUS_BOOT = boot
}

beforeEach(() => {
  FakeSocket.last = undefined
  vi.stubGlobal('WebSocket', FakeSocket)
  at('')
})

afterEach(() => {
  vi.unstubAllGlobals()
  at('')
})

/** Open a carrier and answer its handshake, which every later call rides behind. */
const greeted = async (): Promise<[Carrier, FakeSocket, Seen]> => {
  const [seen, args] = watchers()
  const carrier = new Carrier(...args)
  const opening = carrier.open()
  const socket = FakeSocket.last as FakeSocket
  socket.fire('open')
  await Promise.resolve()
  socket.deliver({ jsonrpc: '2.0', id: socket.idOf(0), result: { protocol_version: '1.0' } })
  await opening
  return [carrier, socket, seen]
}

describe('where the socket is', () => {
  it('takes the manifest the host wrote into the page', async () => {
    at('', { carrier: 'ws://named:1/api/ws', protocol: '1.0' })
    await greeted()
    expect(String(FakeSocket.last?.url)).toBe('ws://named:1/api/ws')
  })

  it('lets ?ws= override the manifest, which is what a dev server needs', async () => {
    at('?ws=ws://override:2/api/ws', { carrier: 'ws://named:1/api/ws', protocol: '1.0' })
    await greeted()
    expect(String(FakeSocket.last?.url)).toBe('ws://override:2/api/ws')
  })

  it('falls back to this page\u2019s own origin when nothing says otherwise', async () => {
    at('')
    await greeted()
    expect(String(FakeSocket.last?.url)).toBe(`ws://${window.location.host}/api/ws`)
  })

  it('uses wss when the page itself is https', async () => {
    // A page served over TLS dialling `ws:` is blocked by the browser as mixed
    // content, so getting this wrong is a panel that never connects at all.
    // jsdom refuses to redefine `location.protocol`, so the whole location is
    // replaced for the length of the case.
    const origin = window.location
    Object.defineProperty(window, 'location', {
      value: { ...origin, protocol: 'https:', host: origin.host, search: '' },
      configurable: true,
      writable: true,
    })
    await greeted()
    expect(String(FakeSocket.last?.url)).toBe(`wss://${origin.host}/api/ws`)
    Object.defineProperty(window, 'location', { value: origin, configurable: true, writable: true })
  })
})

describe('the token', () => {
  it('rides in the query string, because a browser cannot set a handshake header', async () => {
    at('?token=s3cret')
    await greeted()
    expect(String(FakeSocket.last?.url)).toContain('?token=s3cret')
  })

  it('is percent-encoded, so a secret with a & in it is not two parameters', async () => {
    at('?token=a%26b%3Dc')
    await greeted()
    expect(String(FakeSocket.last?.url)).toContain('token=a%26b%3Dc')
  })

  it('comes from the manifest under the published posture', async () => {
    at('', { carrier: 'ws://x/api/ws', protocol: '1.0', token: 'published' })
    await greeted()
    expect(String(FakeSocket.last?.url)).toContain('token=published')
  })

  it('prefers the reader\u2019s own URL over the published one', async () => {
    at('?token=mine', { carrier: 'ws://x/api/ws', protocol: '1.0', token: 'published' })
    await greeted()
    expect(String(FakeSocket.last?.url)).toContain('token=mine')
  })

  it('is absent from the URL when there is none', async () => {
    await greeted()
    expect(String(FakeSocket.last?.url)).not.toContain('token')
  })
})

describe('the handshake', () => {
  it('greets before anything else, which is the codec\u2019s rule', async () => {
    const [, socket] = await greeted()
    expect(socket.methodOf(0)).toBe('rpc.hello')
  })

  it('names the protocol the manifest stated', async () => {
    at('', { carrier: 'ws://x/api/ws', protocol: '9.9' })
    const [, socket] = await greeted()
    const sent = JSON.parse(socket.sent[0] ?? '{}') as { params: { protocol_version: string } }
    expect(sent.params.protocol_version).toBe('9.9')
  })

  it('falls back to 1.0 when the page carries no manifest', async () => {
    const [, socket] = await greeted()
    const sent = JSON.parse(socket.sent[0] ?? '{}') as { params: { protocol_version: string } }
    expect(sent.params.protocol_version).toBe('1.0')
  })

  it('is done once; a second open returns the same promise and opens no socket', async () => {
    const [carrier] = await greeted()
    const first = FakeSocket.last
    await carrier.open()
    expect(FakeSocket.last).toBe(first)
  })

  it('rejects when the socket errors before it opens', async () => {
    const [, args] = watchers()
    const carrier = new Carrier(...args)
    const opening = carrier.open()
    ;(FakeSocket.last as FakeSocket).fire('error')
    await expect(opening).rejects.toMatchObject({ code: -32000 })
  })

  it('rejects when the server refuses the greeting', async () => {
    const [, args] = watchers()
    const carrier = new Carrier(...args)
    const opening = carrier.open()
    const socket = FakeSocket.last as FakeSocket
    socket.fire('open')
    await Promise.resolve()
    socket.deliver({ jsonrpc: '2.0', id: socket.idOf(0), error: { code: -32600, message: 'no' } })
    await expect(opening).rejects.toMatchObject({ code: -32600, message: 'no' })
  })
})

describe('calls', () => {
  it('resolves with the result slot', async () => {
    const [carrier, socket] = await greeted()
    const asked = carrier.call('session.list', {})
    socket.deliver({ jsonrpc: '2.0', id: socket.idOf(1), result: { sessions: [] } })
    await expect(asked).resolves.toEqual({ sessions: [] })
  })

  it('rejects with the error object, so a caller can read the code', async () => {
    const [carrier, socket] = await greeted()
    const asked = carrier.call('agent.prompt', {})
    socket.deliver({ jsonrpc: '2.0', id: socket.idOf(1), error: { code: -32602, message: 'bad' } })
    await expect(asked).rejects.toEqual({ code: -32602, message: 'bad' })
  })

  it('gives every call its own id', async () => {
    const [carrier, socket] = await greeted()
    void carrier.call('a', {})
    void carrier.call('b', {})
    expect(socket.idOf(1)).not.toBe(socket.idOf(2))
  })

  it('refuses before the socket exists rather than sending into nothing', async () => {
    const [, args] = watchers()
    const carrier = new Carrier(...args)
    await expect(carrier.call('anything', {})).rejects.toMatchObject({
      message: 'the carrier is not open',
    })
  })

  it('refuses while the socket is still connecting', async () => {
    const [, args] = watchers()
    const carrier = new Carrier(...args)
    void carrier.open()
    await expect(carrier.call('anything', {})).rejects.toMatchObject({
      message: 'the carrier is not open',
    })
  })

  it('fails everything in flight when the socket closes', async () => {
    // The failure a codec must not have: a caller left waiting on a socket
    // that has gone away waits for ever.
    const [carrier, socket] = await greeted()
    const asked = carrier.call('session.list', {})
    socket.fire('close', { code: 1006, reason: '' })
    await expect(asked).rejects.toMatchObject({
      message: 'the carrier closed while this call was in flight',
    })
  })

  it('a close after the answer does not reject an already-settled call', async () => {
    const [carrier, socket] = await greeted()
    const asked = carrier.call('session.list', {})
    socket.deliver({ jsonrpc: '2.0', id: socket.idOf(1), result: 'fine' })
    await expect(asked).resolves.toBe('fine')
    expect(() => { socket.fire('close', { code: 1000, reason: 'bye' }) }).not.toThrow()
  })
})

describe('frames a hostile or broken peer can send', () => {
  const dropped: Array<[string, unknown]> = [
    ['a frame that is not JSON', 'this is not json'],
    ['a bare number', '7'],
    ['a JSON null', 'null'],
    ['an empty array', '[]'],
  ]

  for (const [name, raw] of dropped) {
    it(`${name} is dropped without throwing`, async () => {
      const [, socket] = await greeted()
      expect(() => { socket.fire('message', { data: raw }) }).not.toThrow()
    })
  }

  it('a binary frame is dropped, because this codec is text only', async () => {
    const [, socket, seen] = await greeted()
    socket.fire('message', { data: new ArrayBuffer(4) })
    expect(seen.events).toEqual([])
  })

  it('a reply to an id nobody asked for is dropped', async () => {
    const [, socket] = await greeted()
    expect(() => { socket.deliver({ jsonrpc: '2.0', id: 9999, result: 'ghost' }) }).not.toThrow()
  })

  it('a reply with a non-numeric id is dropped', async () => {
    const [, socket] = await greeted()
    expect(() => { socket.deliver({ jsonrpc: '2.0', id: 'seven', result: 'x' }) }).not.toThrow()
  })

  it('a reply with no id at all is dropped', async () => {
    const [, socket] = await greeted()
    expect(() => { socket.deliver({ jsonrpc: '2.0', result: 'x' }) }).not.toThrow()
  })

  it('an explicit null error is read as success, not as failure', async () => {
    // `serde` writes `error: null` rather than omitting it in some shapes, and
    // reading that as a failure would fail every successful call.
    const [carrier, socket] = await greeted()
    const asked = carrier.call('x', {})
    socket.deliver({ jsonrpc: '2.0', id: socket.idOf(1), result: 'ok', error: null })
    await expect(asked).resolves.toBe('ok')
  })

  it('the same id answered twice settles once and drops the second', async () => {
    const [carrier, socket] = await greeted()
    const asked = carrier.call('x', {})
    const id = socket.idOf(1)
    socket.deliver({ jsonrpc: '2.0', id, result: 'first' })
    await expect(asked).resolves.toBe('first')
    expect(() => { socket.deliver({ jsonrpc: '2.0', id, result: 'second' }) }).not.toThrow()
  })
})

describe('pushes', () => {
  it('delivers a session/event with its session id', async () => {
    const [, socket, seen] = await greeted()
    const pushed = { type: 'user/message', seq: 1, time: 2, data: {} }
    socket.deliver({ jsonrpc: '2.0', method: 'session/event', params: { session_id: 's1', event: pushed } })
    expect(seen.events).toEqual([['s1', pushed]])
  })

  it('reads the state field, not a boolean nobody sends', async () => {
    const [, socket, seen] = await greeted()
    socket.deliver({ jsonrpc: '2.0', method: 'agent/status', params: { session_id: 's1', state: 'running' } })
    socket.deliver({ jsonrpc: '2.0', method: 'agent/status', params: { session_id: 's1', state: 'idle' } })
    expect(seen.status).toEqual([['s1', true], ['s1', false]])
  })

  it('a state this build has not learned is not running', async () => {
    const [, socket, seen] = await greeted()
    socket.deliver({ jsonrpc: '2.0', method: 'agent/status', params: { session_id: 's', state: 'hibernating' } })
    expect(seen.status).toEqual([['s', false]])
  })

  it('a session/event with no event is dropped rather than delivered empty', async () => {
    const [, socket, seen] = await greeted()
    socket.deliver({ jsonrpc: '2.0', method: 'session/event', params: { session_id: 's1' } })
    socket.deliver({ jsonrpc: '2.0', method: 'session/event', params: { session_id: 's1', event: null } })
    expect(seen.events).toEqual([])
  })

  it('a push with no params at all does not throw', async () => {
    const [, socket] = await greeted()
    expect(() => { socket.deliver({ jsonrpc: '2.0', method: 'agent/status' }) }).not.toThrow()
  })

  it('a push with a non-string session id answers with the empty string', async () => {
    const [, socket, seen] = await greeted()
    socket.deliver({ jsonrpc: '2.0', method: 'agent/status', params: { session_id: 12, state: 'idle' } })
    expect(seen.status).toEqual([['', false]])
  })

  it('a method this screen does not serve is ignored, per \u00a74.3.2', async () => {
    const [, socket, seen] = await greeted()
    socket.deliver({ jsonrpc: '2.0', id: 1, method: 'ui/ask', params: { session_id: 's' } })
    expect(seen.events).toEqual([])
    expect(seen.status).toEqual([])
  })
})

describe('health', () => {
  it('reports connecting, then open, then closed with the reason given', async () => {
    const [, socket, seen] = await greeted()
    socket.fire('close', { code: 1011, reason: 'engine went away' })
    expect(seen.health).toEqual([
      { state: 'connecting' },
      { state: 'open' },
      { state: 'closed', why: 'engine went away' },
    ])
  })

  it('supplies a reason when the peer gave none, so the pill is never blank', async () => {
    const [, socket, seen] = await greeted()
    socket.fire('close', { code: 1006, reason: '' })
    expect(seen.health.at(-1)).toEqual({
      state: 'closed',
      why: 'the carrier closed the connection (1006)',
    })
  })
})

describe('close', () => {
  it('closes the socket and lets a later open start a new one', async () => {
    const [carrier, socket] = await greeted()
    carrier.close()
    expect(socket.closed).toBe(true)
    void carrier.open()
    expect(FakeSocket.last).not.toBe(socket)
  })

  it('closing twice does not throw', async () => {
    const [carrier] = await greeted()
    carrier.close()
    expect(() => { carrier.close() }).not.toThrow()
  })
})
