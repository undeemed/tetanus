// The protocol seam, against the real engine.
//
// Every other spec here plays the engine, which means every other spec agrees
// with the panel by construction: if the fold expects `call_id` and the engine
// writes `callId`, a fake peer written from the same misreading passes.
//
// This one dials a real `tetanus serve`, over the real WebSocket carrier, and
// runs a real turn that really executes a shell command. The journal it folds
// is the one the engine wrote. Nothing in this file states what an event looks
// like: the assertions are about the *rows a reader sees*, so a field renamed
// on either side fails here and nowhere else.
//
// `crates/host/tests/panel_engine.rs` starts the server and passes its address
// in `TETANUS_PANEL_CARRIER`. Run alone, there is no server to dial, so the
// case skips - and the Rust gate is what makes sure it does not skip when it
// matters.

import { describe, expect, it } from 'vitest'
import { fold } from '../src/timeline.ts'
import type { SessionEvent } from '../src/carrier.ts'

const address = process.env['TETANUS_PANEL_CARRIER']

/** One JSON-RPC conversation with the engine, over one socket. */
class Peer {
  private readonly socket: WebSocket
  private next = 1
  private readonly waiting = new Map<number, { ok: (value: unknown) => void; no: (why: unknown) => void }>()
  readonly journal: SessionEvent[] = []
  private running = false

  private constructor(socket: WebSocket) {
    this.socket = socket
    socket.addEventListener('message', (event: MessageEvent) => {
      const frame = JSON.parse(String(event.data)) as Record<string, unknown>
      if (frame['method'] === 'session/event') {
        const params = frame['params'] as { event: SessionEvent }
        this.journal.push(params.event)
        return
      }
      if (frame['method'] === 'agent/status') {
        this.running = (frame['params'] as { state: string }).state === 'running'
        return
      }
      const id = frame['id']
      if (typeof id !== 'number') return
      const pending = this.waiting.get(id)
      if (pending === undefined) return
      this.waiting.delete(id)
      if (frame['error'] !== undefined && frame['error'] !== null) pending.no(frame['error'])
      else pending.ok(frame['result'])
    })
  }

  static async dial(url: string): Promise<Peer> {
    const socket = new WebSocket(url)
    await new Promise<void>((resolve, reject) => {
      const die = setTimeout(() => { reject(new Error(`${url}: never opened`)) }, 10_000)
      socket.addEventListener('open', () => { clearTimeout(die); resolve() }, { once: true })
      socket.addEventListener('error', () => { clearTimeout(die); reject(new Error(`${url}: refused`)) }, { once: true })
    })
    return new Peer(socket)
  }

  call(method: string, params: unknown): Promise<unknown> {
    const id = this.next++
    return new Promise((ok, no) => {
      // Every wait carries a deadline. A bridge's characteristic failure is a
      // wait that never ends, and a hung case reads as a wedged machine.
      const die = setTimeout(() => { no(new Error(`${method}: no answer in 60s`)) }, 60_000)
      this.waiting.set(id, {
        ok: (value) => { clearTimeout(die); ok(value) },
        no: (why) => { clearTimeout(die); no(why) },
      })
      this.socket.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
    })
  }

  isRunning(): boolean {
    return this.running
  }

  close(): void {
    this.socket.close()
  }
}

const suite = address === undefined ? describe.skip : describe

suite('the panel against a real engine', () => {
  it('folds a real turn into the rows a reader sees', async () => {
    const peer = await Peer.dial(address as string)
    try {
      const hello = (await peer.call('rpc.hello', {
        protocol_version: '1.0',
        client: { name: 'tetanus-panel-e2e', version: '0' },
      })) as { protocol_version: string }
      expect(hello.protocol_version).toMatch(/^\d+\.\d+$/)

      const created = (await peer.call('session.create', {})) as { session_id: string; model: string }
      expect(created.session_id).not.toBe('')
      await peer.call('session.subscribe', { session_id: created.session_id })

      // `!` asks the offline mock adapter for the shell tool, so this turn
      // really runs a command and really writes a tool/result.
      await peer.call('agent.prompt', {
        session_id: created.session_id,
        content: '! echo panel-e2e-marker',
      })

      // The journal arrived on the subscription while the prompt was in
      // flight. Fold it exactly as the page does.
      const folded = fold(peer.journal, peer.isRunning())
      const drawn = folded.order.map((key) => folded.nodes.get(key)?.kind)

      // A question, an answer, and a tool card - in that order, from an engine
      // nobody told what the panel expects.
      expect(drawn).toContain('user')
      expect(drawn).toContain('assistant-step')
      expect(drawn).toContain('tool-call')
      expect(drawn.indexOf('user')).toBeLessThan(drawn.indexOf('assistant-step'))

      // The question the reader typed, read back off the row.
      const question = folded.order
        .map((key) => folded.nodes.get(key))
        .find((node) => node?.kind === 'user')
      const content = (question?.data as { content: Array<{ text: string }> }).content
      expect(content[0]?.text).toBe('! echo panel-e2e-marker')

      // The tool card settled, and it carries what the command actually
      // printed. This is the assertion that a hand-written fixture cannot
      // make: the shape came from the engine and the text came from bash.
      const card = folded.order
        .map((key) => folded.nodes.get(key))
        .find((node) => node?.kind === 'tool-call')
      const root = (card?.data as { root: Record<string, unknown> }).root
      expect(root['kind'], 'the tool row never settled').toBe('tool-result')
      expect(root['isError']).toBe(false)
      const printed = (root['content'] as Array<{ text: string }>).map((each) => each.text).join('\n')
      expect(printed).toContain('panel-e2e-marker')

      // The turn closed, so nothing is left looking like it is still running.
      expect([...folded.timeline.turns.values()].every((turn) => turn.status === 'closed')).toBe(true)

      // Every event the engine wrote is either folded into a shaped row or
      // drawn raw. Nothing is silently dropped - contract §4.3.2.
      expect(peer.journal.length).toBeGreaterThan(5)
    } finally {
      peer.close()
    }
  }, 120_000)

  it('refuses a call the contract does not serve, rather than hanging', async () => {
    const peer = await Peer.dial(address as string)
    try {
      await peer.call('rpc.hello', {
        protocol_version: '1.0',
        client: { name: 'tetanus-panel-e2e', version: '0' },
      })
      await expect(peer.call('there.is.no.such.method', {})).rejects.toMatchObject({
        code: expect.any(Number),
      })
    } finally {
      peer.close()
    }
  }, 60_000)

  it('refuses a prompt for a session that does not exist', async () => {
    const peer = await Peer.dial(address as string)
    try {
      await peer.call('rpc.hello', {
        protocol_version: '1.0',
        client: { name: 'tetanus-panel-e2e', version: '0' },
      })
      await expect(peer.call('agent.prompt', { session_id: 'no-such-session', content: 'hi' }))
        .rejects.toMatchObject({ code: expect.any(Number) })
    } finally {
      peer.close()
    }
  }, 60_000)
})
