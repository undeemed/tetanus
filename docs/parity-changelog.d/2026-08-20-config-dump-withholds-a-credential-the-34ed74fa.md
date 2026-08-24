---
date: 2026-08-20
order: 39
---
`config.dump` withholds a credential the settings document holds (`tetanus_config::secret::names_a_secret`, `Catalogs::dump`, TC-SECRET-1..4, TC-CFG-SECRET-1..4). Until now every key the caller resolved was echoed with its value, and the document already has an `llm.providers.<name>.*` namespace, so a key written there was printed by `tetanus config` and answered to whatever client was on a carrier. The rule is the one `docs/interface-contract.md` §4.3 publishes: the last word of the key decides, because the engine has no schema for a key it does not settle.
