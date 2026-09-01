// The keyed node table, without the plugin framework that usually holds it.
//
// Upstream registers one renderer per node kind through cordis
// (`ui-conversation/src/client/chat/register-node-renderers.ts` and
// `ui-tool/src/client/apply.ts`), so `renderSlot('conversation.chat.node', …)`
// is a lookup in a registry a dependency-injection container filled. The
// registry is a map from `kind` to a component; the container is how upstream
// lets a plugin add a row type at runtime.
//
// This panel has no plugins, so it has the map and not the container. That is
// the whole substitution, and it is why nothing here imports cordis: the
// keys and the components are upstream's, unchanged.

import { createElement } from 'react'
import type { ComponentType, ReactNode } from 'react'
import { t } from './locale.ts'
import { AssistantNodeView } from '@deepseek-ai/dsh-client-ui-conversation/client/chat/AssistantNodeView.tsx'
import {
  UnknownNodeView,
  UserMessageNodeView,
} from '@deepseek-ai/dsh-client-ui-conversation/client/chat/MessageItem.tsx'
import { ToolCallTree } from '@deepseek-ai/dsh-client-ui-tool/client/tool/ToolCallTree.tsx'
import { GenericToolCard } from '@deepseek-ai/dsh-client-ui-tool/client/tool/toolviews/GenericToolCard.tsx'

/* eslint-disable @typescript-eslint/no-explicit-any -- upstream's slot props
 * are generic over a merge-extensible registry that only exists inside their
 * workspace program. Reproducing those generics here would be a second guess
 * at a contract nothing checks; the map's correctness is checked by the view
 * rendering, and by `crates/host/tests/panel_port.rs` holding the key set
 * against the engine's vocabulary. */
type Any = any

/** Upstream's node kinds, and the component each one draws with. */
const NODES: Record<string, ComponentType<Any>> = {
  user: UserMessageNodeView,
  'assistant-step': AssistantNodeView,
  'tool-call': ToolCallTree,
  unknown: UnknownNodeView,
}

/** The kinds this build draws, read by the guard rather than re-listed there. */
export const DRAWN: readonly string[] = Object.keys(NODES)

interface SlotOptions {
  entryKey?: string
  hookContext?: string
  fallback?: ReactNode
}

/**
 * The two slots the conversation view dispatches through.
 *
 * `conversation.chat.node` is one row. `tool.call.toolview` is one tool card
 * inside a row - upstream ships a view per tool family and falls back to the
 * generic card, and this build takes the fallback for every tool, which is
 * honest and complete rather than a stub. Which tools want a shaped view is a
 * staged decision, recorded in the report.
 */
export function renderSlot(name: string, owner: Any, options?: SlotOptions): ReactNode {
  if (name === 'tool.call.toolview') return <GenericToolCard {...owner} t={t} />
  if (name !== 'conversation.chat.node') return options?.fallback ?? null
  const draw = options?.entryKey === undefined ? undefined : NODES[options.entryKey]
  if (draw === undefined) return options?.fallback ?? null
  // What upstream's slot framework injects around the owner share: the locale
  // seat, the turn-scoped reader, and the slot function itself so a row that
  // dispatches again (the tool tree does, per subcall) reaches this same map.
  //
  // `createElement` rather than a call, because every one of these is wrapped
  // in `memo` and a memo component is an object React renders, not a function
  // anybody may invoke. Calling one throws `draw is not a function` at the
  // first row that arrives - which is to say the panel looks perfect until the
  // moment it is used.
  return createElement(draw, { ...owner, t, useTurnData, renderSlot })
}

/**
 * The turn-scoped business reader.
 *
 * Upstream's turn tail publishes `turn-tail` here and the assistant row reads
 * it to decide whether it is the closing answer of a settled turn - which
 * drives the branch affordance and the ran-for footer. Neither is served by
 * this screen, so the reader answers nothing rather than a guess, and the
 * assistant row takes its own `undefined` branch.
 */
const useTurnData = (): undefined => undefined
