---
date: 2026-08-19
order: 16
---
The named prompt-section registry implemented (`crates/turn/src/prompt.rs`, service key `system-prompt`) and ported (TC-PORT-PROMPT-8..12). The engine's base prompt is now a registered section like any other. Variables, runtime context and scoped layers named as the remaining `system-prompt` gaps.
