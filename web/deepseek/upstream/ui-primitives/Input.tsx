/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.
 * Vendored verbatim from deepseek-ai/deepseek-harness: packages/client/ui-primitives/src/Input.tsx
 * The full notice is web/deepseek/upstream/LICENSE. Unmodified
 * apart from this header. */
// Input: single-line text input atom (search boxes, inline forms). Composer
// textareas are NOT this atom — they live with the conversation package.

import type { InputHTMLAttributes, ReactNode } from 'react'
import clsx from 'clsx'
import css from './Input.module.css'

/**
 * Render a text input with an optional leading icon.
 * @param props.icon - optional 16px leading icon node.
 * @returns wrapper span containing the native input; input attributes pass through.
 */
export function Input({ icon, className, ...rest }: {
  icon?: ReactNode
  className?: string
} & InputHTMLAttributes<HTMLInputElement>) {
  return (
    <span className={clsx(css.wrap, className)}>
      {icon != null && <span className={css.icon}>{icon}</span>}
      <input className={css.input} {...rest} />
    </span>
  )
}
