/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.
 * Vendored verbatim from deepseek-ai/deepseek-harness: packages/client/runtime/src/client/time-zone.ts
 * The full notice is web/deepseek/upstream/LICENSE. Unmodified
 * apart from this header. */
/** Browser-owned time-zone sampling for prompt RPC provenance. */

/**
 * Resolve the current browser IANA zone for one outbound operation.
 * @returns The browser-provided canonical zone.
 * @throws when the runtime cannot provide a non-empty zone.
 */
export function resolvedClientTimeZone(): string {
  const timeZone = new Intl.DateTimeFormat().resolvedOptions().timeZone
  if (typeof timeZone !== 'string' || timeZone.length === 0) {
    throw new Error('browser time zone is unavailable')
  }
  return timeZone
}
