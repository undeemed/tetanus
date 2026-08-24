---
date: 2026-08-20
order: 57
---
What a settings document overrides ported (`crates/config/tests/upstream_settings_merge.rs`, TC-PORT-SET-1..5). tetanus has no merge step: `file::read` flattens a document into dotted keys and `Config` resolves one key across the layers, so the deep merge upstream performs is a property of the pair and nothing asserted it. The cases pin the four answers a user depends on - one leaf written keeps the rest of its section, a list is replaced whole rather than merged element by element, an empty section sets nothing, and a key written with no value is a null that does not fall back - and the one place the flat model differs from upstream's schema, which section 3 now carries as the open question it is.
