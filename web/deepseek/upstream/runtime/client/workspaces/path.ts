/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.
 * Vendored verbatim from deepseek-ai/deepseek-harness: packages/client/runtime/src/client/workspaces/path.ts
 * The full notice is web/deepseek/upstream/LICENSE. Unmodified
 * apart from this header. */
/**
 * Resolve a workspace-relative path into the Host-facing spelling used by openPath.
 * @param cwd - session workspace root, when known.
 * @param path - absolute or workspace-relative path.
 * @returns an absolute path when a workspace root is available, otherwise the original path.
 */
export function resolveWorkspacePath(cwd: string | undefined, path: string): string {
  if (path.startsWith('/') || /^[A-Za-z]:[/\\]/.test(path) || path.startsWith('\\\\')) return path
  if (cwd === undefined || cwd === '') return path
  const base = cwd.replace(/[/\\]+$/, '')
  const rel = path.replace(/^[/\\]+/, '')
  return `${base}/${rel}`
}
