---
date: 2026-08-20
order: 34
---
A document names the tool order (`crates/engine/src/tools.rs`, key `tools.order`, TC-ORDER-1..5). Until now `TurnConfig::tool_order` was a value only a composer in Rust could set, so the ported order was unreachable from the one place a deployment states what it wants. An order the registry cannot serve is refused when the settings are resolved, with the rule's own message, and `config.dump` publishes the key even when nothing sets it.
