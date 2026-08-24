---
date: 2026-08-19
order: 21
---
A stream that ends without `[DONE]` is refused as `PROTOCOL` (`crates/turn/src/llm/deepseek.rs`), a frame after the sentinel decodes to nothing, and `sse.spec.ts` ported (TC-DS-CLOSE-1..4, TC-DS-DECODE-3). The empty-response classification named as an `llm/*` gap that needs a contract PR before it can land.
