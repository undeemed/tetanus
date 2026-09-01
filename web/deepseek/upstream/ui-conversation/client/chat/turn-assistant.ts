/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.
 * Vendored verbatim from deepseek-ai/deepseek-harness: packages/client/ui-conversation/src/client/chat/turn-assistant.ts
 * The full notice is web/deepseek/upstream/LICENSE. Unmodified
 * apart from this header. */
import type { AssistantBlock } from '@deepseek-ai/dsh-client-runtime/client'

/**
 * Collect visible prose from one Assistant lifecycle.
 * @param blocks - Assistant content blocks.
 * @returns concatenated text blocks.
 */
export function assistantText(blocks: readonly AssistantBlock[]): string {
  return blocks.flatMap(block => block.kind === 'text' ? [block.text] : []).join('')
}
