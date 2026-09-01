/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.
 * Vendored verbatim from deepseek-ai/deepseek-harness: packages/client/ui-primitives/src/Pill.tsx
 * The full notice is web/deepseek/upstream/LICENSE. Unmodified
 * apart from this header. */
// Pill: small rounded label chip (view switcher tabs, filters, badges).

import type { ButtonHTMLAttributes, ReactNode } from 'react'
import clsx from 'clsx'
import css from './Pill.module.css'

/**
 * Render a pill chip. Interactive when onClick is supplied (renders a button);
 * otherwise a static span.
 * @param props.active - selected/active visual state.
 * @returns pill element.
 */
export function Pill({ active = false, className, children, onClick, ...rest }: {
  active?: boolean
  // `| undefined` so a caller can forward an optional class straight through
  // under exactOptionalPropertyTypes (a CSS-module lookup is string|undefined).
  className?: string | undefined
  children?: ReactNode
} & ButtonHTMLAttributes<HTMLButtonElement>) {
  if (!onClick) {
    return <span className={clsx(css.pill, active && css.active, className)}>{children}</span>
  }
  return (
    <button
      type="button"
      className={clsx(css.pill, css.interactive, active && css.active, className)}
      onClick={onClick}
      {...rest}
    >
      {children}
    </button>
  )
}
