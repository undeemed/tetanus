---
date: 2026-08-20
order: 36
---
The prompt-variable registry implemented (`PromptRegistry::variable`, `PromptRegistry::variables`) and ported as TC-PORT-PROMPT-24..26. A name no reference could carry is refused at the registration that made the mistake, not at the render that trips over it. The assembly that carries the variables, so section text is interpolated before the model reads it, is the next slice.
