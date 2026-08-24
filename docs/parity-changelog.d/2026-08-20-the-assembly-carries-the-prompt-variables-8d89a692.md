---
date: 2026-08-20
order: 49
---
The assembly carries the prompt variables (`SystemPrompt::variables`, `SystemPrompt::render`, `AssemblePrompt::variables`; TC-PORT-PROMPT-27..29, TC-FAULT-10). Until now section text reached the model with its `{{name}}` references still in it, because the registry that resolved the values and the render that would have used them were never joined up. Substitution is the last step, so a `system-prompt/assemble` listener may add a name or replace a value and the model reads what it left. A reference the assembly cannot fill is `TurnError::Prompt`, mapped to `Internal`: the request never left, and retrying sends the same sections through the same registry.
