// The store `ChatView` selects from.
//
// Upstream builds this out of cordis services and a session manager. None of
// that is needed to hold three values and tell React when they change, so this
// is a plain zustand store carrying exactly the keys the view reads and
// nothing speculative: `chat.order`, `chat.nodes`, `chat.timeline`, `queue`,
// `running`, `openState`, `openError`, `hasMore`, `loadingOlder`.
//
// Keeping it to what is read is deliberate. A store shaped "like upstream's"
// would be a second guess at their contract that nothing checks; a store
// shaped like the selectors is checked by the view failing to render.

import { create } from 'zustand'
import type { Folded } from './timeline.ts'
import { fold } from './timeline.ts'
import type { Health, SessionEvent } from './carrier.ts'

export interface OpenError {
  message: string
  code: number
}

export interface PanelState {
  sessionId: string
  /** The journal window this page holds, in `seq` order. */
  events: SessionEvent[]
  chat: Folded
  /** Upstream's steering inbox. We serve no queue yet, so it is always empty. */
  queue: never[]
  running: boolean
  openState: 'idle' | 'loading' | 'open' | 'error'
  openError: OpenError | null
  hasMore: boolean
  loadingOlder: boolean
  health: Health
  /** The model this session was composed with, for the header. */
  model: string

  opened: (sessionId: string, model: string, events: SessionEvent[]) => void
  failed: (error: OpenError) => void
  arrived: (event: SessionEvent) => void
  ran: (running: boolean) => void
  healthChanged: (health: Health) => void
}

const EMPTY: Folded = { order: [], nodes: new Map(), timeline: { turnOrder: [], turns: new Map() } }

export const usePanel = create<PanelState>((set, get) => ({
  sessionId: '',
  events: [],
  chat: EMPTY,
  queue: [],
  running: false,
  openState: 'idle',
  openError: null,
  hasMore: false,
  loadingOlder: false,
  health: { state: 'connecting' },
  model: '',

  opened: (sessionId, model, events) => {
    set({ sessionId, model, events, chat: fold(events, get().running), openState: 'open', openError: null })
  },
  failed: (error) => { set({ openState: 'error', openError: error }) },
  arrived: (event) => {
    const state = get()
    // A subscription can replay what the first page already carried, so a seq
    // already held is dropped rather than appended. Without this a reconnect
    // draws every settled answer twice, which reads as the model repeating
    // itself.
    if (state.events.some((each) => each.seq === event.seq)) return
    const events = [...state.events, event]
    set({ events, chat: fold(events, state.running) })
  },
  ran: (running) => {
    const state = get()
    set({ running, chat: fold(state.events, running) })
  },
  healthChanged: (health) => { set({ health }) },
}))
