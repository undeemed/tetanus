/* Copyright (c) 2026 DeepSeek. Licensed under the MIT License.
 * Vendored from deepseek-ai/deepseek-harness: packages/client/ui-primitives/tests/onboarding-surface.client.spec.tsx
 * The full notice is web/deepseek/upstream/LICENSE.
 * Unmodified apart from this header and the relative import paths,
 * which lost their `src/` segment when the sources were vendored one
 * directory shallower. */
// @vitest-environment jsdom
import { cleanup, render } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { OnboardingSurface } from '@deepseek-ai/dsh-client-ui-primitives'

let appRoot: HTMLDivElement

beforeEach(() => {
  appRoot = document.createElement('div')
  appRoot.id = 'root'
  document.body.appendChild(appRoot)
})

afterEach(() => {
  cleanup()
  appRoot.remove()
})

describe('OnboardingSurface', () => {
  it('portals the overlay chrome to document.body around its content', () => {
    const view = render(<OnboardingSurface><p>step content</p></OnboardingSurface>)
    // Portaled: the overlay is a body child, not inside the render container.
    expect(view.container.querySelector('[class*="onboardingOverlay"]')).toBeNull()
    const overlay = document.body.querySelector('[class*="onboardingOverlay"]')
    expect(overlay).not.toBeNull()
    // The onboarding e2e pins the mask by class substring; the stage carries
    // the content.
    expect(overlay!.querySelector('[class*="onboardingMask"]')).not.toBeNull()
    const stage = overlay!.querySelector('[class*="onboardingStage"]')
    expect(stage).not.toBeNull()
    expect(stage!.textContent).toBe('step content')
  })

  it('holds #root inert for exactly its own lifetime', () => {
    const view = render(<OnboardingSurface>x</OnboardingSurface>)
    expect(appRoot.inert).toBe(true)
    view.unmount()
    expect(appRoot.inert).toBe(false)
  })

  it('renders without an #root element (compositions that mount elsewhere)', () => {
    appRoot.remove()
    const view = render(<OnboardingSurface>x</OnboardingSurface>)
    expect(document.body.querySelector('[class*="onboardingStage"]')!.textContent).toBe('x')
    view.unmount()
  })
})
