/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.
 * Vendored verbatim from deepseek-ai/deepseek-harness: packages/client/runtime/src/client/conversation/view-registry.ts
 * The full notice is web/deepseek/upstream/LICENSE. Unmodified
 * apart from this header. */
import type { Context } from '@deepseek-ai/cordis'
import type { ConversationViewDefinition } from '../contract/conversation.ts'
import { ConversationDefinitionRegistry } from './definition-registry.ts'

/** Runtime registry of per-target Conversation snapshot builders. */
export class ConversationViewRegistry extends ConversationDefinitionRegistry<ConversationViewDefinition> {

  /** @param ctx - owning Client Runtime context. */
  constructor(ctx: Context) {
    super(ctx, 'conversationViews')
  }

  /**
   * Register a uniquely named view builder factory for the caller's lifetime.
   * @param definition - target builder contribution.
   * @returns idempotent disposer.
   */
  register(definition: ConversationViewDefinition): () => void {
    return this.registerDefinition(
      definition.target,
      definition,
      `conversation view target "${definition.target}" is already registered`,
      `conversationViews.register(${JSON.stringify(definition.target)})`,
    )
  }
}
