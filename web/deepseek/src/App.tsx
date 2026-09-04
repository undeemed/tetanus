// The screen: upstream's conversation view, our engine behind it.
//
// One session, opened on load, prompted from the composer, streaming through
// the carrier. Everything visible below the header bar is upstream's
// `ChatView` and the renderers it dispatches to; everything above it is the
// minimum chrome a single screen needs to be usable at all, and is explicitly
// not a port of upstream's shell - that is staged, and the report says so.

import { useCallback, useEffect, useRef, useState } from 'react'
import { ChatView } from '@deepseek-ai/dsh-client-ui-conversation/client/chat/ChatView.tsx'
import { Carrier } from './carrier.ts'
import type { Health, RpcFailure, SessionEvent } from './carrier.ts'
import { usePanel } from './store.ts'
import { renderSlot } from './renderers.tsx'
import { t } from './locale.ts'
import { PRODUCT, TAGLINE } from './brand.ts'
import css from './App.module.css'

/** How much of a cold journal the first page asks for. */
const WINDOW = 500

const failureOf = (error: unknown): RpcFailure => {
  const said = error as Partial<RpcFailure> | undefined
  return {
    code: typeof said?.code === 'number' ? said.code : -32000,
    message: typeof said?.message === 'string' ? said.message : String(error),
  }
}

export function App() {
  const state = usePanel()
  const carrier = useRef<Carrier | null>(null)
  const [asked, setAsked] = useState('')

  useEffect(() => {
    const panel = usePanel.getState()
    const live = new Carrier(
      (_session, event: SessionEvent) => { usePanel.getState().arrived(event) },
      (_session, running) => { usePanel.getState().ran(running) },
      (health: Health) => { usePanel.getState().healthChanged(health) },
    )
    carrier.current = live
    let stopped = false

    const open = async (): Promise<void> => {
      await live.open()
      // A session and a subscription, in that order: subscribing to an id the
      // engine has not minted is the one ordering that cannot work.
      const created = (await live.call('session.create', {})) as Record<string, unknown>
      const sessionId = String(created['session_id'] ?? '')
      const model = String(created['model'] ?? '')
      await live.call('session.subscribe', { session_id: sessionId })
      // The window the page starts from. A brand-new session answers empty and
      // that is the normal case; opening an existing journal is what makes the
      // call worth making at all.
      const page = (await live.call('session.events', {
        session_id: sessionId,
        limit: WINDOW,
      })) as Record<string, unknown>
      const events = Array.isArray(page['events']) ? (page['events'] as SessionEvent[]) : []
      if (!stopped) usePanel.getState().opened(sessionId, model, events)
    }

    panel.healthChanged({ state: 'connecting' })
    usePanel.setState({ openState: 'loading' })
    open().catch((error: unknown) => {
      if (!stopped) usePanel.getState().failed(failureOf(error))
    })
    return () => {
      stopped = true
      live.close()
    }
  }, [])

  const send = useCallback(() => {
    const text = asked.trim()
    const live = carrier.current
    if (text === '' || live === null || state.sessionId === '') return
    setAsked('')
    usePanel.getState().ran(true)
    // `agent.prompt` answers when the turn closes; the transcript arrives
    // meanwhile on the subscription, which is the whole reason this is not
    // awaited for the rendering.
    live
      .call('agent.prompt', { session_id: state.sessionId, content: text })
      .catch((error: unknown) => { usePanel.getState().failed(failureOf(error)) })
      .finally(() => { usePanel.getState().ran(false) })
  }, [asked, state.sessionId])

  const stop = useCallback(() => {
    const live = carrier.current
    if (live === null || state.sessionId === '') return
    live.call('agent.interrupt', { session_id: state.sessionId }).catch(() => {})
  }, [state.sessionId])

  return (
    <div className={css.root}>
      <header className={css.bar}>
        <span className={css.mark}>{PRODUCT}</span>
        <span className={css.tagline}>{TAGLINE}</span>
        <span className={css.spacer} />
        {state.model !== '' && <span className={css.model}>{state.model}</span>}
        <span className={css.health} data-state={state.health.state}>
          {state.health.state === 'open' ? 'connected' : state.health.state}
        </span>
      </header>

      <main className={css.body} data-conversation-scroll>
        <ChatView
          {...chatProps(state.sessionId)}
        />
      </main>

      <footer className={css.composer}>
        <textarea
          className={css.input}
          value={asked}
          rows={2}
          placeholder={t('placeholder.default')}
          onChange={(event) => { setAsked(event.target.value) }}
          onKeyDown={(event) => {
            if (event.key === 'Enter' && !event.shiftKey) {
              event.preventDefault()
              send()
            }
          }}
        />
        {state.running
          ? <button type="button" className={css.stop} onClick={stop}>{t('input.stop')}</button>
          : <button type="button" className={css.send} onClick={send} disabled={asked.trim() === ''}>
            {t('input.send')}
          </button>}
      </footer>
    </div>
  )
}

/* eslint-disable @typescript-eslint/no-explicit-any -- see renderers.tsx: the
 * slot props are generic over a registry that only exists inside upstream's
 * own program. */
type Any = any

/**
 * What upstream's `ChatView` is handed.
 *
 * Exported for its own test. Half of these are seats this screen honours and
 * leaves empty, and an empty seat is still a contract: `chatScroll.read`
 * returning `undefined` instead of `null` is a crash inside upstream, not a
 * missing feature, and nothing reaches it through the DOM.
 *
 * Written out rather than spread from a context, because the list *is* the
 * coupling to upstream: a prop they add fails here loudly on the next refresh
 * instead of arriving as `undefined` three components down.
 */
export function chatProps(sessionId: string): Any {
  return {
    sessionId,
    // The three stores upstream separates. This panel serves one session, so
    // two of them read the same state and the third is the details selection,
    // which no screen here opens yet.
    useSession: usePanel,
    useSessions: (select: (state: Any) => unknown) =>
      select({ byId: { [sessionId]: { cwd: undefined } } }),
    useStore: (select: (state: Any) => unknown) => select({ selection: undefined }),
    renderSlot,
    t,
    openFile: () => {},
    inspectCall: () => {},
    forkAt: () => {},
    loadOlder: () => {},
    loadImage: () => Promise.reject(new Error(t('image.serviceUnavailable'))),
    fileMentions: () => undefined,
    // Upstream persists the reader's scroll position across a view-tab switch.
    // One screen has no tabs to switch, so the seat is honoured and empty
    // rather than faked: `read` says there is nothing saved, every time.
    chatScroll: { read: () => null, save: () => {} },
  }
}
