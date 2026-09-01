/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.
 * Vendored verbatim from deepseek-ai/deepseek-harness: packages/client/ui-conversation/src/client/chat/use-throttled-visual-update.ts
 * The full notice is web/deepseek/upstream/LICENSE. Unmodified
 * apart from this header. */
/** Frame-throttled scheduling for non-essential visual alignment. */
import { useCallback, useLayoutEffect, useRef } from 'react'

const DEFAULT_INTERVAL_FRAMES = 3

/**
 * Return a stable scheduler that coalesces visual updates over a frame interval.
 * @param update - DOM alignment to run after the throttle interval.
 * @param intervalFrames - frames to wait before applying the latest alignment.
 * @returns a stable function that schedules the latest update.
 */
export function useThrottledVisualUpdate(
  update: () => void,
  intervalFrames = DEFAULT_INTERVAL_FRAMES,
): () => void {
  const updateRef = useRef(update)
  updateRef.current = update
  const pendingFrameRef = useRef<number | null>(null)

  useLayoutEffect(() => () => {
    if (pendingFrameRef.current === null) return
    cancelAnimationFrame(pendingFrameRef.current)
    pendingFrameRef.current = null
  }, [])

  return useCallback(() => {
    if (pendingFrameRef.current !== null) return
    let remainingFrames = intervalFrames
    const advance = (): void => {
      remainingFrames -= 1
      if (remainingFrames > 0) {
        pendingFrameRef.current = requestAnimationFrame(advance)
        return
      }
      pendingFrameRef.current = null
      updateRef.current()
    }
    pendingFrameRef.current = requestAnimationFrame(advance)
  }, [intervalFrames])
}
