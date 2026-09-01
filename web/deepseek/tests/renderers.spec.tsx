// @vitest-environment jsdom
//
// The keyed node table, and the locale seat it injects.
//
// The regression this file exists for: every one of upstream's node views is
// wrapped in `memo`, and a memo component is an OBJECT React renders rather
// than a function anybody may call. Calling one throws `draw is not a
// function` at the first row that arrives - so the panel loads, connects, and
// dies on the first message. It shipped exactly that way once during this
// port, and no structural check saw it.

import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import { DRAWN, renderSlot } from '../src/renderers.tsx'
import { t } from '../src/locale.ts'

afterEach(cleanup)

/** The owner share `ChatNodeSeat` hands a row, with a node of one kind. */
const owner = (kind: string, data: unknown) => ({
  node: { key: 'k', kind, id: 'k', target: 'chat', anchorSeq: 0, data, visibility: 'visible', location: { kind: 'step', turn: 1, step: 1, status: 'closed' } },
  selectedCallId: undefined,
  cwd: undefined,
  openFile: () => {},
  inspectCall: () => {},
  forkAt: () => {},
  loadImage: () => Promise.reject(new Error('no images here')),
  fileMentions: () => undefined,
})

describe('the table', () => {
  it('names exactly the kinds the fold produces', () => {
    expect([...DRAWN].sort()).toEqual(['assistant-step', 'tool-call', 'unknown', 'user'])
  })
})

describe('conversation.chat.node', () => {
  it('renders a user row through upstream\u2019s own component', () => {
    render(<>{renderSlot('conversation.chat.node', owner('user', {
      kind: 'user', seq: 0, time: 1_700_000_000_000, content: [{ type: 'text', text: 'hello there' }], source: {},
    }), { entryKey: 'user' })}</>)
    expect(screen.getByText('hello there')).toBeTruthy()
  })

  it('renders a settled assistant row', () => {
    render(<>{renderSlot('conversation.chat.node', owner('assistant-step', {
      status: 'settled', turn: 1, step: 1, time: 1, blocks: [{ kind: 'text', text: 'an answer' }],
    }), { entryKey: 'assistant-step' })}</>)
    expect(screen.getByText('an answer')).toBeTruthy()
  })

  it('renders a running assistant row without a final node', () => {
    // The streaming state reaches a different branch of upstream's view: it
    // has no `finalNode`, so the turn-tail correlation must not be attempted.
    expect(() => render(<>{renderSlot('conversation.chat.node', owner('assistant-step', {
      status: 'running', turn: 1, step: 1, time: 1, blocks: [{ kind: 'text', text: 'part' }],
    }), { entryKey: 'assistant-step' })}</>)).not.toThrow()
    expect(screen.getByText('part')).toBeTruthy()
  })

  it('renders a running tool row', () => {
    render(<>{renderSlot('conversation.chat.node', owner('tool-call', {
      root: { callId: 'c1', name: 'shell', argsRaw: '{"command":"ls"}', turn: 1, step: 1, time: 1, callView: null, subCalls: [] },
    }), { entryKey: 'tool-call' })}</>)
    expect(document.querySelector('[data-chat-call-id="c1"]')).toBeTruthy()
  })

  it('renders a settled tool row and marks its state', () => {
    render(<>{renderSlot('conversation.chat.node', owner('tool-call', {
      root: {
        kind: 'tool-result', callId: 'c1', seq: 2, time: 2, callTime: 1,
        call: { name: 'shell', argsRaw: '{"command":"ls"}' },
        content: [{ type: 'text', text: 'files' }], isError: false,
        callView: null, resultView: null, subCalls: [],
      },
    }), { entryKey: 'tool-call' })}</>)
    expect(document.querySelector('[data-state="ok"]')).toBeTruthy()
  })

  it('marks a failed tool row as an error', () => {
    render(<>{renderSlot('conversation.chat.node', owner('tool-call', {
      root: {
        kind: 'tool-result', callId: 'c1', seq: 2, time: 2, callTime: 1,
        call: { name: 'shell', argsRaw: '{}' },
        content: [{ type: 'text', text: 'boom' }], isError: true,
        callView: null, resultView: null, subCalls: [],
      },
    }), { entryKey: 'tool-call' })}</>)
    expect(document.querySelector('[data-state="error"]')).toBeTruthy()
  })

  it('renders an unknown row as a labelled payload', () => {
    render(<>{renderSlot('conversation.chat.node', owner('unknown', {
      kind: 'unknown', seq: 3, time: 1, type: 'quantum/entangled', payload: { spin: 'up' },
    }), { entryKey: 'unknown' })}</>)
    expect(screen.getByText(/quantum\/entangled/)).toBeTruthy()
  })

  it('a kind with no renderer takes the fallback the seat supplied', () => {
    const drawn = renderSlot('conversation.chat.node', owner('nope', {}), {
      entryKey: 'nope', fallback: <span>fell back</span>,
    })
    render(<>{drawn}</>)
    expect(screen.getByText('fell back')).toBeTruthy()
  })

  it('a kind with no renderer and no fallback draws nothing rather than throwing', () => {
    expect(renderSlot('conversation.chat.node', owner('nope', {}), { entryKey: 'nope' })).toBeNull()
  })

  it('no entry key at all draws nothing', () => {
    expect(renderSlot('conversation.chat.node', owner('user', {}))).toBeNull()
  })
})

describe('tool.call.toolview', () => {
  it('draws the generic card for every tool, which is this build\u2019s decision', () => {
    render(<>{renderSlot('tool.call.toolview', {
      callId: 'c1',
      toolName: 'shell',
      block: { callId: 'c1', name: 'shell', argsRaw: '{"command":"ls"}', turn: 1, step: 1, time: 1, callView: null, subCalls: [] },
      openFile: () => {},
      cwd: undefined,
      inspect: () => {},
    })}</>)
    expect(document.querySelector('[data-tool="shell"]')).toBeTruthy()
  })
})

describe('a slot this panel does not serve', () => {
  it('takes the fallback', () => {
    render(<>{renderSlot('conversation.session.header', {}, { fallback: <span>nothing here</span> })}</>)
    expect(screen.getByText('nothing here')).toBeTruthy()
  })

  it('draws nothing when there is no fallback', () => {
    expect(renderSlot('conversation.details.tool', {})).toBeNull()
  })
})

describe('the locale seat', () => {
  it('answers from upstream\u2019s own dictionary', () => {
    expect(t('input.send')).toBe('Send message')
  })

  it('fills a placeholder', () => {
    expect(t('json.truncated', { total: 12 })).toContain('12')
  })

  it('leaves a placeholder alone when nothing supplies it', () => {
    expect(t('json.truncated', {})).toContain('{total}')
  })

  it('answers a key it does not know with the key, so the gap is visible', () => {
    expect(t('no.such.key')).toBe('no.such.key')
  })

  it('an unknown key with params is still the key', () => {
    expect(t('no.such.key', { a: 1 })).toBe('no.such.key')
  })
})
