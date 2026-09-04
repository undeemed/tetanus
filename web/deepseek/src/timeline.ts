// Our journal, folded into the shape upstream's conversation view reads.
//
// This file is the port. Everything else is plumbing.
//
// # Why the seam is here and not at the HTTP path
//
// The obvious-looking port is to serve upstream's `/api` gateway from
// `crates/host` so their client runs untouched. Two facts rule it out. Their
// gateway is twelve domains and about 3,100 lines of contract - sessions,
// subagents, workspaces, skills, presets, goals, settings, credentials, llm,
// downloads, host, events - against our fifteen methods, and reimplementing it
// in Rust buys nothing, because the components would still need their whole
// client runtime. And their client runtime does not consume that gateway
// directly: it consumes `ConversationTimelineSnapshot` and a keyed node store,
// which their own session projection derives from their event log.
//
// So the honest seam is the derived one. `ChatView` never asks where a node
// came from; it reads `chat.order`, `chat.nodes` and `chat.timeline`. This
// file produces those three from the durable types our engine actually writes
// (`docs/interface-contract.md` §4.3.1), which is the smallest true statement
// of the gap between the two projects.
//
// # The fold is total
//
// §4.3.2 says the vocabulary grows and a surface renders what it does not
// know. Every event this fold does not name lands on an `unknown` node rather
// than being dropped, so a type added to the engine tomorrow shows up as a raw
// card instead of vanishing. `KNOWN` below is what has a drawn shape; the
// difference between it and the engine's vocabulary is asserted by
// `crates/host/tests/panel_port.rs`, not left to a reader.

import type { SessionEvent } from './carrier.ts'

/** Upstream's per-row identity: `ChatConversationViewNode`. */
export interface ChatNode {
  readonly key: string
  readonly kind: string
  readonly id: string
  readonly target: 'chat'
  readonly anchorSeq: number
  readonly location: { kind: 'turn' | 'step' | 'session'; turn: number; step?: number; status: 'open' | 'closed' | 'unknown' }
  readonly visibility: 'visible'
  readonly data: unknown
}

/** Upstream's `AssistantBlock`. */
export type AssistantBlock =
  | { kind: 'text'; text: string }
  | { kind: 'reasoning'; text: string }
  | { kind: 'tool-call'; callId: string; name: string; argsRaw: string }
  | { kind: 'other'; block: unknown }

interface StepLocation {
  turn: number
  step: number
  start: SessionEvent | undefined
  end: SessionEvent | undefined
  status: 'open' | 'closed' | 'unknown'
  data: { get: () => undefined }
}

interface TurnLocation {
  turn: number
  start: SessionEvent | undefined
  end: SessionEvent | undefined
  status: 'open' | 'closed' | 'unknown'
  steps: StepLocation[]
  data: { get: () => undefined }
}

/** Upstream's `ConversationTimelineSnapshot`. */
export interface Timeline {
  turnOrder: readonly number[]
  turns: Map<number, TurnLocation>
}

export interface Folded {
  order: string[]
  nodes: Map<string, ChatNode>
  timeline: Timeline
}

/**
 * The durable types this fold draws with a shape of their own.
 *
 * Exported because the Rust guard reads it: the difference between this set
 * and what the engine writes is what TC-WEB-10's successor reports, and a set
 * that lived only in a `switch` could not be read from outside.
 */
export const KNOWN: readonly string[] = [
  'session/start',
  'turn/start',
  'turn/end',
  'step/start',
  'step/end',
  'user/message',
  'assistant/chunk',
  'assistant/message',
  'tool/call',
  'tool/result',
]

/** Types that are folded into another row rather than drawn as one. */
const STRUCTURAL = new Set(['session/start', 'turn/start', 'turn/end', 'step/start', 'step/end'])

const asRecord = (value: unknown): Record<string, unknown> =>
  typeof value === 'object' && value !== null ? (value as Record<string, unknown>) : {}

const asNumber = (value: unknown, fallback: number): number =>
  typeof value === 'number' ? value : fallback

const asString = (value: unknown): string => (typeof value === 'string' ? value : '')

/** A running tool call, before its result lands: upstream's `RunningToolCall`. */
interface RunningCall {
  callId: string
  name: string
  argsRaw: string
  turn: number
  step: number
  time: number
  callView: null
  subCalls: readonly never[]
}

/**
 * A settled call: upstream's `ToolResultNode`.
 *
 * Every field is theirs, including the two that look redundant. `kind` is the
 * discriminator that tells a running call from a settled one - `'kind' in
 * block` is literally how `toolRowModel` decides - and `isError` rather than
 * an `ok` of our own is what drives the failed state, because that is the name
 * their model reads. A near-miss here is a card that renders and is quietly
 * always green.
 */
interface SettledCall {
  kind: 'tool-result'
  callId: string
  seq: number
  time: number
  /** When the paired `tool/call` was logged, for the row's duration. */
  callTime: number | null
  call: { name: string; argsRaw: string } | null
  content: ReadonlyArray<{ type: 'text'; text: string }>
  isError: boolean
  error?: { name: string; code: string }
  callView: null
  resultView: null
  subCalls: readonly never[]
}

/**
 * Fold a journal into the three values `ChatView` reads.
 *
 * Deliberately a fresh fold over the whole window rather than an incremental
 * mutation. The window is bounded by the page the engine served, the cost is
 * linear, and the alternative - a mutable store patched per event - is the
 * shape that drifts from a replay. `crates/turn/src/projections.rs` makes the
 * same call on the engine's side and for the same reason.
 */
export function fold(events: readonly SessionEvent[], running: boolean): Folded {
  const order: string[] = []
  const nodes = new Map<string, ChatNode>()
  const turns = new Map<number, TurnLocation>()
  const turnOrder: number[] = []

  /** The step a bare event belongs to, carried by the last `step/start`. */
  let turn = 0
  let step = 0
  /** The assistant row currently streaming, so a chunk grows it in place. */
  let streamingKey: string | null = null
  /** Calls seen but not yet settled, so a result can replace the right row. */
  const openCalls = new Map<string, string>()

  const locate = (status: 'open' | 'closed' | 'unknown' = 'unknown') =>
    ({ kind: 'step' as const, turn, step, status })

  const put = (node: ChatNode): void => {
    if (!nodes.has(node.key)) order.push(node.key)
    nodes.set(node.key, node)
  }

  const turnAt = (at: number): TurnLocation => {
    let found = turns.get(at)
    if (found === undefined) {
      found = { turn: at, start: undefined, end: undefined, status: 'unknown', steps: [], data: { get: () => undefined } }
      turns.set(at, found)
      turnOrder.push(at)
    }
    return found
  }

  for (const event of events) {
    const data = asRecord(event.data)
    switch (event.type) {
      case 'turn/start': {
        turn = asNumber(data['turn'], turn)
        const at = turnAt(turn)
        at.start = event
        at.status = 'open'
        break
      }
      case 'turn/end': {
        const at = turnAt(asNumber(data['turn'], turn))
        at.end = event
        at.status = 'closed'
        streamingKey = null
        break
      }
      case 'step/start': {
        turn = asNumber(data['turn'], turn)
        step = asNumber(data['step'], step)
        const at = turnAt(turn)
        at.steps.push({ turn, step, start: event, end: undefined, status: 'open', data: { get: () => undefined } })
        streamingKey = null
        break
      }
      case 'step/end': {
        const at = turnAt(asNumber(data['turn'], turn))
        const found = at.steps.find((each) => each.step === asNumber(data['step'], step))
        if (found !== undefined) {
          found.end = event
          found.status = 'closed'
        }
        break
      }
      case 'user/message': {
        const key = `user:${event.seq}`
        put({
          key,
          kind: 'user',
          id: key,
          target: 'chat',
          anchorSeq: event.seq,
          location: locate(),
          visibility: 'visible',
          data: {
            kind: 'user',
            seq: event.seq,
            time: event.time,
            content: userContent(data['content']),
            source: event,
          },
        })
        break
      }
      case 'assistant/chunk': {
        // §4.3.1: `chunk` is `text`, `reasoning` or `tool_call`; the first two
        // carry `delta`. A tool-call chunk is the model *saying* the call and
        // is left to `tool/call`, which is the event that means it ran.
        const which = asString(data['chunk'])
        if (which !== 'text' && which !== 'reasoning') break
        const key: string = streamingKey ?? `assistant:${turn}:${step}`
        streamingKey = key
        const existing = nodes.get(key)
        const blocks = existing === undefined
          ? []
          : [...(asRecord(existing.data)['blocks'] as AssistantBlock[])]
        grow(blocks, which, asString(data['delta']))
        put({
          key,
          kind: 'assistant-step',
          id: key,
          target: 'chat',
          anchorSeq: existing?.anchorSeq ?? event.seq,
          location: locate('open'),
          visibility: 'visible',
          data: { status: 'running', turn, step, blocks, time: event.time },
        })
        break
      }
      case 'assistant/message': {
        // The settled answer replaces whatever streamed into place, keyed the
        // same so the row is updated rather than duplicated. That the two
        // agree is the point of streaming at all.
        const key: string = streamingKey ?? `assistant:${turn}:${step}`
        streamingKey = null
        const blocks = settledBlocks(data)
        const final = {
          kind: 'assistant',
          seq: event.seq,
          time: event.time,
          turn,
          step,
          blocks,
          usage: data['usage'],
        }
        put({
          key,
          kind: 'assistant-step',
          id: key,
          target: 'chat',
          anchorSeq: nodes.get(key)?.anchorSeq ?? event.seq,
          location: locate('closed'),
          visibility: 'visible',
          data: { status: 'settled', turn, step, blocks, time: event.time, finalNode: final },
        })
        break
      }
      case 'tool/call': {
        const callId = asString(data['id'])
        const key = `tool:${callId === '' ? event.seq : callId}`
        openCalls.set(callId, key)
        const call: RunningCall = {
          callId,
          name: asString(data['name']),
          argsRaw: argumentsOf(data['arguments']),
          turn,
          step,
          time: event.time,
          callView: null,
          subCalls: [],
        }
        put({
          key,
          kind: 'tool-call',
          id: key,
          target: 'chat',
          anchorSeq: event.seq,
          location: locate('open'),
          visibility: 'visible',
          data: { root: call },
        })
        break
      }
      case 'tool/result': {
        const callId = asString(data['call_id'])
        const key = openCalls.get(callId) ?? `tool:${callId === '' ? event.seq : callId}`
        openCalls.delete(callId)
        const before = nodes.get(key)
        const started = before === undefined ? undefined : (asRecord(before.data)['root'] as RunningCall)
        const failed = data['ok'] === false
        // §4.3.1: `code` rides a result the engine synthesized or refused
        // rather than ran, and upstream's `stopped` state is exactly the
        // interrupted one - so the code is passed through under the name their
        // model looks for instead of being flattened into "it failed".
        const code = asString(data['code'])
        const settled: SettledCall = {
          kind: 'tool-result',
          callId,
          seq: event.seq,
          time: event.time,
          callTime: started?.time ?? null,
          call: {
            name: asString(data['name']) || (started?.name ?? ''),
            argsRaw: started?.argsRaw ?? '',
          },
          content: resultContent(data['content']),
          isError: failed,
          ...(code === '' ? {} : { error: { name: asString(data['name']), code } }),
          callView: null,
          resultView: null,
          subCalls: [],
        }
        put({
          key,
          kind: 'tool-call',
          id: key,
          target: 'chat',
          anchorSeq: before?.anchorSeq ?? event.seq,
          location: locate('closed'),
          visibility: 'visible',
          data: { root: settled },
        })
        break
      }
      default: {
        if (STRUCTURAL.has(event.type)) break
        // The whole point of §4.3.2: a type this build has not learned is
        // shown, not swallowed. `unknown` is upstream's own fallback kind and
        // its renderer draws the payload as a labelled JSON tree.
        const key = `raw:${event.seq}`
        put({
          key,
          kind: 'unknown',
          id: key,
          target: 'chat',
          anchorSeq: event.seq,
          location: locate(),
          visibility: 'visible',
          data: { kind: 'unknown', seq: event.seq, time: event.time, type: event.type, payload: event.data },
        })
      }
    }
  }

  // A turn that never closed while the agent is still working is open; one
  // left dangling by a crash is not, and the difference is what stops the
  // running label from sticking on a reloaded transcript.
  if (!running) {
    for (const [, at] of turns) if (at.status === 'open') at.status = 'closed'
  }

  return { order, nodes, timeline: { turnOrder, turns } }
}

/**
 * Append a delta to the last block of its kind, or start one.
 *
 * Upstream's own accumulator does the same, and the reason is visible in the
 * rendering: text and reasoning alternate, and a delta that started a fresh
 * block every time would draw one paragraph per token.
 */
function grow(blocks: AssistantBlock[], which: 'text' | 'reasoning', delta: string): void {
  const last = blocks[blocks.length - 1]
  if (last !== undefined && last.kind === which) {
    blocks[blocks.length - 1] = { kind: which, text: last.text + delta }
    return
  }
  blocks.push({ kind: which, text: delta })
}

/** §4.3.1: `assistant/message` carries `content`, `reasoning` and `tool_calls`. */
function settledBlocks(data: Record<string, unknown>): AssistantBlock[] {
  const blocks: AssistantBlock[] = []
  const reasoning = asString(data['reasoning'])
  if (reasoning !== '') blocks.push({ kind: 'reasoning', text: reasoning })
  const content = asString(data['content'])
  if (content !== '') blocks.push({ kind: 'text', text: content })
  const calls = data['tool_calls']
  if (Array.isArray(calls)) {
    for (const each of calls) {
      const call = asRecord(each)
      blocks.push({
        kind: 'tool-call',
        callId: asString(call['id']),
        name: asString(call['name']),
        argsRaw: argumentsOf(call['arguments']),
      })
    }
  }
  return blocks
}

/**
 * A tool result's content, as the blocks upstream's `resultText` walks.
 *
 * It reads `block.type === 'text'` and stringifies anything else, so a bare
 * string - which is what our engine writes for most tools - has to be lifted
 * into one block rather than handed over as-is. Handed over as-is it renders
 * as a JSON dump of every character.
 */
function resultContent(value: unknown): Array<{ type: 'text'; text: string }> {
  if (typeof value === 'string') return [{ type: 'text', text: value }]
  if (Array.isArray(value)) {
    return value.map((each) => {
      const block = asRecord(each)
      return {
        type: 'text' as const,
        text: typeof block['text'] === 'string' ? block['text'] : JSON.stringify(each, null, 2),
      }
    })
  }
  if (value === undefined || value === null) return []
  return [{ type: 'text', text: JSON.stringify(value, null, 2) }]
}

/** Upstream renders arguments as the raw string the model produced. */
function argumentsOf(value: unknown): string {
  if (typeof value === 'string') return value
  if (value === undefined || value === null) return ''
  return JSON.stringify(value)
}

/** §4.3.1: `user/message` carries `content`. Upstream wants content blocks. */
function userContent(value: unknown): Array<{ type: 'text'; text: string }> {
  if (typeof value === 'string') return [{ type: 'text', text: value }]
  if (Array.isArray(value)) {
    return value.map((each) => {
      const block = asRecord(each)
      return { type: 'text' as const, text: asString(block['text']) || JSON.stringify(each) }
    })
  }
  return [{ type: 'text', text: JSON.stringify(value ?? '') }]
}
