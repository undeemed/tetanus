// The page's one entry point.

import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'

// Upstream's design tokens first: every vendored component styles itself from
// `--dsw-*` and nothing else, so a page that loaded a component without these
// would draw a correctly structured, entirely colourless transcript.
import '@deepseek-ai/dsh-client-ui-theme/styles/base.css'
import '@deepseek-ai/dsh-client-ui-theme/styles/design-platform.css'
import '@deepseek-ai/dsh-client-ui-theme/styles/scrollbar.css'
import '@deepseek-ai/dsh-client-ui-theme/styles/shiki.css'

import { App } from './App.tsx'
import { TITLE } from './brand.ts'

document.title = TITLE

const seat = document.getElementById('root')
if (seat === null) throw new Error('the page has no #root to mount into')
createRoot(seat).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
