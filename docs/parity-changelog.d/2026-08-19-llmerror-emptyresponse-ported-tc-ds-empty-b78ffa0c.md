---
date: 2026-08-19
order: 31
---
`LlmError::EmptyResponse` ported (TC-DS-EMPTY-1..3, TC-PORT-RETRY-10): a DeepSeek completion that ends on a clean `stop` with no text, no reasoning and no tool call is a failure rather than a blank answer. `EMPTY_RESPONSE` was already in the default retryable codes with nothing able to raise it, so the defaults now match what the adapter can produce.
