---
date: 2026-08-19
order: 28
---
Strict `{{name}}` interpolation implemented (`prompt::interpolate`) and ported as TC-PORT-PROMPT-16..23. A reference to an unregistered name, to a registered name with no value, and text that opened a reference it never closed are all refused; a lone `{{` with no `}}` after it stays prose, and a substituted value is never scanned again. The registry that holds the variables, and the assembly that carries them to the model, are the next two slices.
