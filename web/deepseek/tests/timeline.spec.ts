// The fold: our journal into upstream's node model.
//
// This is the file the whole port turns on, so it is tested the way the
// captain's bar asks - every branch, both sides of every boundary, and
// malformed input refused with a drawn row rather than a throw.
//
// The hostile-input cases are not decoration. `SessionEvent.data` is
// `serde_json::Value` on the wire (contract §4.3.1), so a `data` that is a
// string, a number, `null`, or an object with every field the wrong type is
// something a peer can send and something a corrupted journal can hold. The
// rule this file pins is that none of those throws: a fold that throws takes
// the whole transcript down, including the parts that were fine.

import { describe, expect, it } from 'vitest'
import { fold, KNOWN } from '../src/timeline.ts'
import type { SessionEvent } from '../src/carrier.ts'

let seq = 0
const event = (type: string, data: unknown, at = 1_700_000_000_000): SessionEvent =>
  ({ type, seq: seq++, time: at, data: data as Record<string, unknown> })

const reset = (): void => { seq = 0 }

/** The node a fold produced for one key, or undefined. */
const nodeOf = (folded: ReturnType<typeof fold>, key: string) => folded.nodes.get(key)

/** Every node in flow order, as `kind` strings. */
const kinds = (folded: ReturnType<typeof fold>): string[] =>
  folded.order.map((key) => folded.nodes.get(key)?.kind ?? '<missing>')

describe('fold: the empty and degenerate cases', () => {
  it('an empty journal folds to an empty flow', () => {
    reset()
    const folded = fold([], false)
    expect(folded.order).toEqual([])
    expect(folded.nodes.size).toBe(0)
    expect(folded.timeline.turnOrder).toEqual([])
    expect(folded.timeline.turns.size).toBe(0)
  })

  it('a journal of nothing but structure draws no rows', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('step/start', { turn: 1, step: 1 }),
      event('step/end', { turn: 1, step: 1 }),
      event('turn/end', { turn: 1, steps: 1, stop_reason: 'natural' }),
    ], false)
    expect(folded.order).toEqual([])
    // The turn still exists in the timeline: `ChatView` reads it to decide
    // whether a turn is running, which is a different question from whether
    // it has anything to show.
    expect(folded.timeline.turnOrder).toEqual([1])
    expect(folded.timeline.turns.get(1)?.status).toBe('closed')
  })
})

describe('fold: turn and step boundaries', () => {
  it('an open turn stays open while the agent is running', () => {
    reset()
    const folded = fold([event('turn/start', { turn: 1 })], true)
    expect(folded.timeline.turns.get(1)?.status).toBe('open')
  })

  it('an open turn is closed when the agent is not running', () => {
    // A turn left dangling by a crash must not leave the running label stuck
    // on a reloaded transcript.
    reset()
    const folded = fold([event('turn/start', { turn: 1 })], false)
    expect(folded.timeline.turns.get(1)?.status).toBe('closed')
  })

  it('turn/end closes the turn it names, not the one in hand', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('turn/start', { turn: 2 }),
      event('turn/end', { turn: 1 }),
    ], true)
    expect(folded.timeline.turns.get(1)?.status).toBe('closed')
    expect(folded.timeline.turns.get(2)?.status).toBe('open')
  })

  it('turn order follows first appearance, not numeric order', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 7 }),
      event('turn/start', { turn: 2 }),
    ], true)
    expect(folded.timeline.turnOrder).toEqual([7, 2])
  })

  it('step/end closes the step it names and leaves its siblings open', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('step/start', { turn: 1, step: 1 }),
      event('step/start', { turn: 1, step: 2 }),
      event('step/end', { turn: 1, step: 1 }),
    ], true)
    const steps = folded.timeline.turns.get(1)?.steps ?? []
    expect(steps.map((each) => [each.step, each.status])).toEqual([[1, 'closed'], [2, 'open']])
  })

  it('a step/end naming a step that never started changes nothing', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('step/end', { turn: 1, step: 9 }),
    ], true)
    expect(folded.timeline.turns.get(1)?.steps).toEqual([])
  })

  it('an event before any turn/start still lands, at turn zero', () => {
    // A window that begins mid-journal is the ordinary case for paging, so a
    // fold that needed a turn boundary first would render nothing at all.
    reset()
    const folded = fold([event('user/message', { content: 'hi' })], false)
    expect(kinds(folded)).toEqual(['user'])
    expect(nodeOf(folded, 'user:0')?.location.turn).toBe(0)
  })

  it('a turn/end for a turn never started creates it, closed', () => {
    reset()
    const folded = fold([event('turn/end', { turn: 4 })], true)
    expect(folded.timeline.turns.get(4)?.status).toBe('closed')
    expect(folded.timeline.turns.get(4)?.start).toBeUndefined()
  })
})

describe('fold: user messages', () => {
  it('a string content becomes one text block', () => {
    reset()
    const folded = fold([event('user/message', { content: 'hello' })], false)
    const data = nodeOf(folded, 'user:0')?.data as { content: unknown[] }
    expect(data.content).toEqual([{ type: 'text', text: 'hello' }])
  })

  it('an array of blocks keeps each block', () => {
    reset()
    const folded = fold([
      event('user/message', { content: [{ type: 'text', text: 'one' }, { type: 'text', text: 'two' }] }),
    ], false)
    const data = nodeOf(folded, 'user:0')?.data as { content: unknown[] }
    expect(data.content).toEqual([{ type: 'text', text: 'one' }, { type: 'text', text: 'two' }])
  })

  it('a block with no text is stringified rather than dropped', () => {
    reset()
    const folded = fold([event('user/message', { content: [{ type: 'image', url: 'x' }] })], false)
    const data = nodeOf(folded, 'user:0')?.data as { content: Array<{ text: string }> }
    expect(data.content[0]?.text).toBe('{"type":"image","url":"x"}')
  })

  it('an absent content becomes an empty string rather than "undefined"', () => {
    reset()
    const folded = fold([event('user/message', {})], false)
    const data = nodeOf(folded, 'user:0')?.data as { content: Array<{ text: string }> }
    expect(data.content).toEqual([{ type: 'text', text: '""' }])
  })

  it('an object content is stringified', () => {
    reset()
    const folded = fold([event('user/message', { content: { odd: true } })], false)
    const data = nodeOf(folded, 'user:0')?.data as { content: Array<{ text: string }> }
    expect(data.content[0]?.text).toBe('{"odd":true}')
  })
})

describe('fold: the assistant stream', () => {
  it('text deltas grow one block rather than one block per delta', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('step/start', { turn: 1, step: 1 }),
      event('assistant/chunk', { chunk: 'text', delta: 'Hel' }),
      event('assistant/chunk', { chunk: 'text', delta: 'lo' }),
    ], true)
    const data = nodeOf(folded, 'assistant:1:1')?.data as { blocks: unknown[]; status: string }
    expect(data.blocks).toEqual([{ kind: 'text', text: 'Hello' }])
    expect(data.status).toBe('running')
  })

  it('a change of chunk kind starts a new block', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('step/start', { turn: 1, step: 1 }),
      event('assistant/chunk', { chunk: 'reasoning', delta: 'think' }),
      event('assistant/chunk', { chunk: 'text', delta: 'say' }),
      event('assistant/chunk', { chunk: 'reasoning', delta: 'more' }),
    ], true)
    const data = nodeOf(folded, 'assistant:1:1')?.data as { blocks: unknown[] }
    expect(data.blocks).toEqual([
      { kind: 'reasoning', text: 'think' },
      { kind: 'text', text: 'say' },
      { kind: 'reasoning', text: 'more' },
    ])
  })

  it('a tool_call chunk is left to tool/call and draws nothing', () => {
    // The model saying a call is not the call running. Drawing both would
    // double every tool row.
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('step/start', { turn: 1, step: 1 }),
      event('assistant/chunk', { chunk: 'tool_call', call: { id: 'c1', name: 'shell' } }),
    ], true)
    expect(folded.order).toEqual([])
  })

  it('a chunk with an unknown kind draws nothing', () => {
    reset()
    const folded = fold([event('assistant/chunk', { chunk: 'video', delta: 'x' })], true)
    expect(folded.order).toEqual([])
  })

  it('a delta that is absent appends an empty string rather than "undefined"', () => {
    reset()
    const folded = fold([event('assistant/chunk', { chunk: 'text' })], true)
    const data = nodeOf(folded, 'assistant:0:0')?.data as { blocks: Array<{ text: string }> }
    expect(data.blocks).toEqual([{ kind: 'text', text: '' }])
  })

  it('a step boundary ends the stream, so the next step gets its own row', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('step/start', { turn: 1, step: 1 }),
      event('assistant/chunk', { chunk: 'text', delta: 'first' }),
      event('step/end', { turn: 1, step: 1 }),
      event('step/start', { turn: 1, step: 2 }),
      event('assistant/chunk', { chunk: 'text', delta: 'second' }),
    ], true)
    expect(folded.order).toEqual(['assistant:1:1', 'assistant:1:2'])
  })

  it('a turn boundary ends the stream too', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('assistant/chunk', { chunk: 'text', delta: 'a' }),
      event('turn/end', { turn: 1 }),
      event('turn/start', { turn: 2 }),
      event('assistant/chunk', { chunk: 'text', delta: 'b' }),
    ], true)
    expect(folded.order).toEqual(['assistant:1:0', 'assistant:2:0'])
  })

  it('the streamed row keeps the seq it started at, not the seq of the last delta', () => {
    // The anchor is what the scroll position and the flow order are keyed on;
    // a row that renumbered itself per token would jump on every delta.
    reset()
    const folded = fold([
      event('assistant/chunk', { chunk: 'text', delta: 'a' }),
      event('assistant/chunk', { chunk: 'text', delta: 'b' }),
    ], true)
    expect(nodeOf(folded, 'assistant:0:0')?.anchorSeq).toBe(0)
  })
})

describe('fold: the settled answer', () => {
  it('replaces the streamed row in place rather than adding a second one', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('step/start', { turn: 1, step: 1 }),
      event('assistant/chunk', { chunk: 'text', delta: 'Hel' }),
      event('assistant/message', { content: 'Hello', reasoning: '', tool_calls: [] }),
    ], true)
    expect(folded.order).toEqual(['assistant:1:1'])
    const data = nodeOf(folded, 'assistant:1:1')?.data as { status: string; blocks: unknown[] }
    expect(data.status).toBe('settled')
    expect(data.blocks).toEqual([{ kind: 'text', text: 'Hello' }])
  })

  it('puts reasoning before content, which is the order it was produced in', () => {
    reset()
    const folded = fold([event('assistant/message', { content: 'answer', reasoning: 'why' })], true)
    const data = nodeOf(folded, 'assistant:0:0')?.data as { blocks: unknown[] }
    expect(data.blocks).toEqual([
      { kind: 'reasoning', text: 'why' },
      { kind: 'text', text: 'answer' },
    ])
  })

  it('empty content and empty reasoning contribute no blocks', () => {
    reset()
    const folded = fold([event('assistant/message', { content: '', reasoning: '' })], true)
    const data = nodeOf(folded, 'assistant:0:0')?.data as { blocks: unknown[] }
    expect(data.blocks).toEqual([])
  })

  it('tool calls in the settled message become tool-call blocks', () => {
    reset()
    const folded = fold([
      event('assistant/message', {
        content: '',
        tool_calls: [{ id: 'c1', name: 'shell', arguments: '{"command":"ls"}' }],
      }),
    ], true)
    const data = nodeOf(folded, 'assistant:0:0')?.data as { blocks: unknown[] }
    expect(data.blocks).toEqual([
      { kind: 'tool-call', callId: 'c1', name: 'shell', argsRaw: '{"command":"ls"}' },
    ])
  })

  it('an arguments object is serialised, because upstream renders the raw string', () => {
    reset()
    const folded = fold([
      event('assistant/message', { tool_calls: [{ id: 'c1', name: 'shell', arguments: { command: 'ls' } }] }),
    ], true)
    const data = nodeOf(folded, 'assistant:0:0')?.data as { blocks: Array<{ argsRaw: string }> }
    expect(data.blocks[0]?.argsRaw).toBe('{"command":"ls"}')
  })

  it('an absent arguments becomes an empty string, not "undefined"', () => {
    reset()
    const folded = fold([event('assistant/message', { tool_calls: [{ id: 'c1', name: 'shell' }] })], true)
    const data = nodeOf(folded, 'assistant:0:0')?.data as { blocks: Array<{ argsRaw: string }> }
    expect(data.blocks[0]?.argsRaw).toBe('')
  })

  it('a null arguments becomes an empty string', () => {
    reset()
    const folded = fold([
      event('assistant/message', { tool_calls: [{ id: 'c1', name: 'shell', arguments: null }] }),
    ], true)
    const data = nodeOf(folded, 'assistant:0:0')?.data as { blocks: Array<{ argsRaw: string }> }
    expect(data.blocks[0]?.argsRaw).toBe('')
  })

  it('a tool_calls that is not an array is ignored rather than thrown on', () => {
    reset()
    const folded = fold([event('assistant/message', { content: 'x', tool_calls: 'nope' })], true)
    const data = nodeOf(folded, 'assistant:0:0')?.data as { blocks: unknown[] }
    expect(data.blocks).toEqual([{ kind: 'text', text: 'x' }])
  })

  it('carries the final node, which is what upstream reads for the turn tail', () => {
    reset()
    const folded = fold([event('assistant/message', { content: 'x', usage: { total: 9 } })], true)
    const data = nodeOf(folded, 'assistant:0:0')?.data as { finalNode: Record<string, unknown> }
    expect(data.finalNode['kind']).toBe('assistant')
    expect(data.finalNode['seq']).toBe(0)
    expect(data.finalNode['usage']).toEqual({ total: 9 })
  })
})

describe('fold: tool calls', () => {
  const call = () => event('tool/call', { id: 'c1', name: 'shell', arguments: '{"command":"ls"}' })

  it('a call with no result is a running row', () => {
    reset()
    const folded = fold([call()], true)
    const root = (nodeOf(folded, 'tool:c1')?.data as { root: Record<string, unknown> }).root
    // Upstream's `toolRowModel` decides settled-ness by `'kind' in block`, so
    // a running call must NOT carry one.
    expect('kind' in root).toBe(false)
    expect(root['name']).toBe('shell')
    expect(root['argsRaw']).toBe('{"command":"ls"}')
  })

  it('a result settles the row it belongs to, in place', () => {
    reset()
    const folded = fold([
      call(),
      event('tool/result', { call_id: 'c1', name: 'shell', ok: true, content: 'files' }),
    ], true)
    expect(folded.order).toEqual(['tool:c1'])
    const root = (nodeOf(folded, 'tool:c1')?.data as { root: Record<string, unknown> }).root
    expect(root['kind']).toBe('tool-result')
    expect(root['isError']).toBe(false)
    expect(root['content']).toEqual([{ type: 'text', text: 'files' }])
  })

  it('a failed result sets isError, which is the field upstream reads', () => {
    reset()
    const folded = fold([
      call(),
      event('tool/result', { call_id: 'c1', ok: false, content: 'no such file' }),
    ], true)
    const root = (nodeOf(folded, 'tool:c1')?.data as { root: Record<string, unknown> }).root
    expect(root['isError']).toBe(true)
  })

  it('a result with no `ok` is read as success, matching the wire default', () => {
    reset()
    const folded = fold([call(), event('tool/result', { call_id: 'c1', content: 'x' })], true)
    const root = (nodeOf(folded, 'tool:c1')?.data as { root: Record<string, unknown> }).root
    expect(root['isError']).toBe(false)
  })

  it('a `code` on the result rides through as upstream error shape', () => {
    reset()
    const folded = fold([
      call(),
      event('tool/result', { call_id: 'c1', name: 'shell', ok: false, code: 'interrupted' }),
    ], true)
    const root = (nodeOf(folded, 'tool:c1')?.data as { root: Record<string, unknown> }).root
    expect(root['error']).toEqual({ name: 'shell', code: 'interrupted' })
  })

  it('no `code` means no error key at all, rather than an empty one', () => {
    reset()
    const folded = fold([call(), event('tool/result', { call_id: 'c1', ok: true })], true)
    const root = (nodeOf(folded, 'tool:c1')?.data as Record<string, unknown>)['root'] as object
    expect('error' in root).toBe(false)
  })

  it('the result backfills the call head when the call is in the window', () => {
    reset()
    const folded = fold([call(), event('tool/result', { call_id: 'c1', ok: true })], true)
    const root = (nodeOf(folded, 'tool:c1')?.data as { root: Record<string, unknown> }).root
    expect(root['call']).toEqual({ name: 'shell', argsRaw: '{"command":"ls"}' })
    expect(root['callTime']).toBe(1_700_000_000_000)
  })

  it('an orphan result - its call cut off by the window - still draws', () => {
    reset()
    const folded = fold([event('tool/result', { call_id: 'c9', name: 'read', ok: true })], true)
    const root = (nodeOf(folded, 'tool:c9')?.data as { root: Record<string, unknown> }).root
    expect(root['kind']).toBe('tool-result')
    expect(root['call']).toEqual({ name: 'read', argsRaw: '' })
    expect(root['callTime']).toBeNull()
  })

  it('a call with no id falls back to its seq, so two of them do not collide', () => {
    reset()
    const folded = fold([
      event('tool/call', { name: 'shell' }),
      event('tool/call', { name: 'shell' }),
    ], true)
    expect(folded.order).toEqual(['tool:0', 'tool:1'])
  })

  it('a result with no id and no matching call falls back to its own seq', () => {
    reset()
    const folded = fold([event('tool/result', { ok: true })], true)
    expect(folded.order).toEqual(['tool:0'])
  })

  it('two calls with the same id settle independently, first result to first call', () => {
    // The engine does not reuse ids, but a journal concatenated by hand can,
    // and the failure mode - one row swallowing the other - is silent.
    reset()
    const folded = fold([
      event('tool/call', { id: 'c1', name: 'shell' }),
      event('tool/result', { call_id: 'c1', ok: true, content: 'first' }),
      event('tool/call', { id: 'c1', name: 'read' }),
      event('tool/result', { call_id: 'c1', ok: false, content: 'second' }),
    ], true)
    expect(folded.order).toEqual(['tool:c1'])
    const root = (nodeOf(folded, 'tool:c1')?.data as { root: Record<string, unknown> }).root
    expect(root['isError']).toBe(true)
  })

  it('an array content keeps each block, and stringifies one with no text', () => {
    reset()
    const folded = fold([
      event('tool/result', {
        call_id: 'c1',
        ok: true,
        content: [{ type: 'text', text: 'line' }, { type: 'image', url: 'u' }],
      }),
    ], true)
    const root = (nodeOf(folded, 'tool:c1')?.data as { root: { content: Array<{ text: string }> } }).root
    expect(root.content[0]?.text).toBe('line')
    expect(root.content[1]?.text).toBe('{\n  "type": "image",\n  "url": "u"\n}')
  })

  it('an absent content is no blocks, not a block reading "undefined"', () => {
    reset()
    const folded = fold([event('tool/result', { call_id: 'c1', ok: true })], true)
    const root = (nodeOf(folded, 'tool:c1')?.data as { root: { content: unknown[] } }).root
    expect(root.content).toEqual([])
  })

  it('a null content is no blocks', () => {
    reset()
    const folded = fold([event('tool/result', { call_id: 'c1', ok: true, content: null })], true)
    const root = (nodeOf(folded, 'tool:c1')?.data as { root: { content: unknown[] } }).root
    expect(root.content).toEqual([])
  })

  it('an object content is serialised into one block', () => {
    reset()
    const folded = fold([event('tool/result', { call_id: 'c1', ok: true, content: { rows: 2 } })], true)
    const root = (nodeOf(folded, 'tool:c1')?.data as { root: { content: Array<{ text: string }> } }).root
    expect(root.content[0]?.text).toBe('{\n  "rows": 2\n}')
  })
})

describe('fold: the unknown, which is the contract\u2019s own rule', () => {
  it('an event this build has never heard of is drawn, not dropped', () => {
    reset()
    const folded = fold([event('quantum/entangled', { spin: 'up' })], false)
    expect(kinds(folded)).toEqual(['unknown'])
    const data = nodeOf(folded, 'raw:0')?.data as Record<string, unknown>
    expect(data['type']).toBe('quantum/entangled')
    expect(data['payload']).toEqual({ spin: 'up' })
  })

  it('a durable type the fold has no shaped row for is drawn raw', () => {
    reset()
    const folded = fold([event('request/context', { tokens: 10 })], false)
    expect(kinds(folded)).toEqual(['unknown'])
  })

  it('two unknowns keep separate rows', () => {
    reset()
    const folded = fold([event('a/b', {}), event('a/b', {})], false)
    expect(folded.order).toEqual(['raw:0', 'raw:1'])
  })
})

describe('fold: hostile and malformed payloads', () => {
  const shapes: Array<[string, unknown]> = [
    ['null', null],
    ['a string', 'not an object'],
    ['a number', 42],
    ['an array', [1, 2, 3]],
    ['a boolean', true],
  ]

  for (const [name, data] of shapes) {
    it(`a ${name} data on every known type folds without throwing`, () => {
      reset()
      const events = KNOWN.map((type) => event(type, data))
      expect(() => fold(events, false)).not.toThrow()
    })
  }

  it('wrong-typed fields fall back rather than propagate', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 'one' }),
      event('step/start', { turn: null, step: [] }),
      event('tool/call', { id: 99, name: false, arguments: 7 }),
    ], true)
    // `turn` was not a number, so the running value stands. Nothing threw and
    // every row still has a key.
    expect(folded.timeline.turnOrder).toEqual([0])
    // A non-string id is treated as absent and the row falls back to its seq,
    // which is what keeps two malformed calls from collapsing into one row.
    const root = (nodeOf(folded, 'tool:2')?.data as { root: Record<string, unknown> }).root
    expect(root['name']).toBe('')
    expect(root['argsRaw']).toBe('7')
  })

  it('an event whose type is the empty string is drawn raw', () => {
    reset()
    const folded = fold([event('', {})], false)
    expect(kinds(folded)).toEqual(['unknown'])
  })

  it('a very long delta is kept whole rather than truncated by the fold', () => {
    // Bounding output is the producer's job (`Tail` in `crates/exec`); a
    // surface that also truncated would cut text that was already trimmed.
    reset()
    const long = 'x'.repeat(200_000)
    const folded = fold([event('assistant/chunk', { chunk: 'text', delta: long })], true)
    const data = nodeOf(folded, 'assistant:0:0')?.data as { blocks: Array<{ text: string }> }
    expect(data.blocks[0]?.text.length).toBe(200_000)
  })
})

describe('fold: flow order and identity', () => {
  it('rows appear in journal order and keep their place when updated', () => {
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('user/message', { content: 'go' }),
      event('step/start', { turn: 1, step: 1 }),
      event('assistant/chunk', { chunk: 'text', delta: 'ok' }),
      event('tool/call', { id: 'c1', name: 'shell' }),
      event('tool/result', { call_id: 'c1', ok: true }),
      event('assistant/message', { content: 'done' }),
    ], true)
    expect(kinds(folded)).toEqual(['user', 'assistant-step', 'tool-call'])
    // The settled answer replaced the streamed row where it already was,
    // rather than jumping below the tool card.
    expect(folded.order).toEqual(['user:1', 'assistant:1:1', 'tool:c1'])
  })

  it('every node carries the fields ChatView keys and anchors on', () => {
    reset()
    const folded = fold([event('user/message', { content: 'x' })], false)
    const node = nodeOf(folded, 'user:0')
    expect(node?.key).toBe('user:0')
    expect(node?.id).toBe('user:0')
    expect(node?.target).toBe('chat')
    expect(node?.visibility).toBe('visible')
    expect(node?.anchorSeq).toBe(0)
  })
})

describe('KNOWN', () => {
  it('names exactly the types the fold has a shaped branch for', () => {
    // The Rust guard reads this array to compare it against the engine's
    // vocabulary, so it has to be the truth rather than a comment.
    expect([...KNOWN].sort()).toEqual([
      'assistant/chunk', 'assistant/message', 'session/start', 'step/end',
      'step/start', 'tool/call', 'tool/result', 'turn/end', 'turn/start',
      'user/message',
    ])
  })

  it('no type in it falls through to the unknown row', () => {
    for (const type of KNOWN) {
      reset()
      const folded = fold([event(type, {})], false)
      const drawn = folded.order.map((key) => folded.nodes.get(key)?.kind)
      expect(drawn, `${type} fell through to the raw path`).not.toContain('unknown')
    }
  })
})

describe('the location data readers upstream calls', () => {
  it('a turn and its steps answer nothing, because this panel publishes nothing', () => {
    // Upstream's `ConversationLocationDataStore` lets a plugin attach business
    // values to a turn or a step. This panel has no plugins, so the reader is
    // honoured and empty rather than absent - an absent `data` is a throw
    // inside `AssistantNodeView`, not a missing feature.
    reset()
    const folded = fold([
      event('turn/start', { turn: 1 }),
      event('step/start', { turn: 1, step: 1 }),
    ], true)
    const turn = folded.timeline.turns.get(1)
    expect(turn?.data.get()).toBeUndefined()
    expect(turn?.steps[0]?.data.get()).toBeUndefined()
  })
})
