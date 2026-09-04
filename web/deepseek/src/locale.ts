// The locale seat, over upstream's own dictionary.
//
// Upstream's `t` comes from a cordis locale service that merges a dictionary
// per plugin namespace and picks a language. Every string the conversation
// view asks for lives in one namespace - `conversation`, which `ui-tool`
// shares by design (`upstream/ui-tool/client/locale.ts`) - so the merge has
// one input and the service reduces to a lookup with `{name}` interpolation.
//
// The dictionary is upstream's, not a re-typed copy: a key they add or reword
// arrives with the next refresh instead of silently answering its own name.

import { en } from '@deepseek-ai/dsh-client-ui-conversation/client/locales.ts'

type Dictionary = Record<string, string>

/**
 * Look one key up and fill its placeholders.
 *
 * A key with no entry answers with itself. That is deliberate: a missing
 * string should be visible in the interface as the key that is missing, not
 * hidden behind an empty span that reads as a layout bug.
 */
export function t(key: string, params?: Record<string, unknown>): string {
  const said = (en as unknown as Dictionary)[key] ?? key
  if (params === undefined) return said
  return said.replace(/\{(\w+)\}/g, (whole, name: string) =>
    name in params ? String(params[name]) : whole)
}
