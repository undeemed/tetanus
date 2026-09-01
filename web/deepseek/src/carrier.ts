// The engine, over the carrier this repository already serves.
//
// This is the whole of the answer to "how does upstream's UI talk to our
// engine". Upstream's client dials its own `/api/<domain>.<verb>` gateway and
// reads two WebSocket downlinks; ours speaks JSON-RPC 2.0 on one socket at
// `/api/ws`, per `docs/interface-contract.md` §4.1. Rather than teach the Rust
// host a second protocol, the client's transport is replaced - which is the
// smaller half by a wide margin, because our contract has 15 methods and
// upstream's gateway has twelve domains.
//
// Nothing above this file knows there is a socket. It hands out one `call` and
// one push callback, both in the vocabulary `tetanus-protocol` publishes.

/** What the host's index tap wrote into the page (`crates/host/src/lib.rs`). */
interface Boot {
  carrier: string
  protocol: string
  token?: string
}

declare global {
  interface Window {
    TETANUS_BOOT?: Boot
  }
}

/** One durable journal line, contract §4.3. */
export interface SessionEvent {
  type: string
  seq: number
  time: number
  data: Record<string, unknown>
  sourceEventSeqs?: number[]
}

export interface RpcFailure {
  code: number
  message: string
  data?: unknown
}

/** What the socket tells the page about itself. */
export type Health =
  | { state: 'connecting' }
  | { state: 'open' }
  | { state: 'closed'; why: string }

interface Waiting {
  resolve: (value: unknown) => void
  reject: (error: RpcFailure) => void
}

/**
 * Where the socket is.
 *
 * The manifest is authoritative because a page opened through a proxy is told
 * where to go by whoever put it there (`crates/cli/src/web.rs`). `?ws=` is the
 * override a developer serving the page from vite needs, and the same-origin
 * guess is the last resort so the page still works when opened directly.
 */
function carrierUrl(): string {
  const stated = new URLSearchParams(window.location.search).get('ws')
  if (stated !== null) return stated
  const manifest = window.TETANUS_BOOT?.carrier
  if (manifest !== undefined) return manifest
  const scheme = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${scheme}//${window.location.host}/api/ws`
}

/**
 * The secret, when the deployment has one.
 *
 * Two postures, both from `crates/cli/src/web.rs`: `--token` keeps it in the
 * reader's own URL and out of the page, and `--open-to-anyone` publishes it in
 * the manifest. A browser cannot set a header on a WebSocket handshake
 * (§4.1.2), so it rides the query string either way.
 */
function token(): string | null {
  const stated = new URLSearchParams(window.location.search).get('token')
  return stated ?? window.TETANUS_BOOT?.token ?? null
}

/** A live connection to the engine. */
export class Carrier {
  private socket: WebSocket | null = null
  private nextId = 1
  private readonly waiting = new Map<number, Waiting>()
  private greeted: Promise<unknown> | null = null

  constructor(
    /** Every `session/event` push, in arrival order. */
    private readonly onEvent: (sessionId: string, event: SessionEvent) => void,
    /** Every `agent/status` push. */
    private readonly onStatus: (sessionId: string, running: boolean) => void,
    private readonly onHealth: (health: Health) => void,
  ) {}

  /**
   * Open the socket and complete the handshake.
   *
   * `rpc.hello` first is the codec's rule, not a courtesy: `crates/rpc` refuses
   * every other method until it has been greeted, so the greeting is awaited
   * once here and every later call rides behind it.
   */
  open(): Promise<unknown> {
    if (this.greeted !== null) return this.greeted
    this.onHealth({ state: 'connecting' })
    const secret = token()
    const base = carrierUrl()
    const url = secret === null ? base : `${base}?token=${encodeURIComponent(secret)}`
    const socket = new WebSocket(url)
    this.socket = socket

    this.greeted = new Promise((resolve, reject) => {
      socket.addEventListener('open', () => {
        this.onHealth({ state: 'open' })
        this.call('rpc.hello', {
          protocol_version: window.TETANUS_BOOT?.protocol ?? '1.0',
          client: { name: 'tetanus-panel', version: '0.1.0' },
        }).then(resolve, reject)
      })
      socket.addEventListener('error', () => {
        reject({ code: -32000, message: `the carrier at ${base} refused the connection` })
      })
    })

    socket.addEventListener('message', (event) => { this.receive(event.data) })
    socket.addEventListener('close', (event) => {
      this.onHealth({
        state: 'closed',
        why: event.reason === '' ? `the carrier closed the connection (${event.code})` : event.reason,
      })
      // Everything still in flight is now unanswerable. Failing them is the
      // one thing a codec must do here: a caller left waiting on a socket that
      // has gone away waits for ever.
      for (const [, pending] of this.waiting) {
        pending.reject({ code: -32000, message: 'the carrier closed while this call was in flight' })
      }
      this.waiting.clear()
    })
    return this.greeted
  }

  /** One request, one answer. */
  call(method: string, params: unknown): Promise<unknown> {
    const socket = this.socket
    if (socket === null || socket.readyState !== WebSocket.OPEN) {
      return Promise.reject({ code: -32000, message: 'the carrier is not open' } satisfies RpcFailure)
    }
    const id = this.nextId++
    return new Promise((resolve, reject) => {
      this.waiting.set(id, { resolve, reject })
      socket.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
    })
  }

  close(): void {
    this.socket?.close()
    this.socket = null
    this.greeted = null
  }

  /**
   * One inbound frame.
   *
   * Both directions arrive here: an answer to something this page asked, and a
   * push the engine started. §4.1 says both peers demultiplex with the same
   * envelope, so the discriminator is whether the frame carries a `method`.
   */
  private receive(raw: unknown): void {
    if (typeof raw !== 'string') return
    let parsed: unknown
    try {
      parsed = JSON.parse(raw)
    } catch {
      // A frame that is not JSON is the carrier's fault and not something a
      // reader can act on; dropping it is what leaves the page usable.
      return
    }
    // Valid JSON is not the same as a message. `null`, `7` and `[]` all parse,
    // and `null` in particular is four bytes that would otherwise take the
    // whole socket down on the property read below - `typeof null` is
    // `'object'`, so the obvious check is the one that does not catch it.
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return
    const frame = parsed as Record<string, unknown>
    const method = frame['method']
    if (typeof method === 'string') {
      this.push(method, frame['params'])
      return
    }
    const id = frame['id']
    if (typeof id !== 'number') return
    const pending = this.waiting.get(id)
    if (pending === undefined) return
    this.waiting.delete(id)
    const failure = frame['error']
    if (failure !== undefined && failure !== null) {
      pending.reject(failure as RpcFailure)
      return
    }
    pending.resolve(frame['result'])
  }

  /** A server-initiated frame: §4.4.3's notifications. */
  private push(method: string, params: unknown): void {
    const payload = (params ?? {}) as Record<string, unknown>
    // Snake case on the wire: the protocol crate derives serde's default and
    // renames only `sourceEventSeqs` (`crates/protocol/src/types.rs`).
    const sessionId = typeof payload['session_id'] === 'string' ? payload['session_id'] : ''
    if (method === 'session/event') {
      const event = payload['event']
      if (event !== undefined && event !== null) this.onEvent(sessionId, event as SessionEvent)
      return
    }
    if (method === 'agent/status') {
      this.onStatus(sessionId, payload['state'] === 'running')
    }
    // Anything else - `ui/ask`, `ui/approve` - is a wait this screen does not
    // serve yet. §4.3.2's rule is to pass an unknown through rather than fail
    // on it, and the staged plan says which screen takes them.
  }
}
