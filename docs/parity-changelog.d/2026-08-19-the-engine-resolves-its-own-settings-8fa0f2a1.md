---
date: 2026-08-19
order: 29
---
The engine resolves its own settings out of the document (`crates/engine/src/boot.rs`, TC-BOOT-1..5): the four keys `catalog::key` names, over the compiled defaults, with a value of the wrong type refused rather than ignored. Until now the reader in `crates/config` had no caller outside its own tests, so `config.dump` reported provenance for keys no document could set. Calling it from the binary is the presentation lane's wiring.
