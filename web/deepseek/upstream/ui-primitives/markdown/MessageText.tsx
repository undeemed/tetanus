/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.
 * Vendored verbatim from deepseek-ai/deepseek-harness: packages/client/ui-primitives/src/markdown/MessageText.tsx
 * The full notice is web/deepseek/upstream/LICENSE. Unmodified
 * apart from this header. */
// MessageText is the literal-text primitive for user and steering content; assistant output uses MarkdownText.

import css from './MessageText.module.css'

export function MessageText({ text }: { text: string }) {
  return <div className={css.text}>{text}</div>
}
