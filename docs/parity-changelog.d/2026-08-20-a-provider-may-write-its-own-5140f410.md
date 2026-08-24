---
date: 2026-08-20
order: 37
---
A provider may write its own retry block (`retry::provider_policy`, `retry::provider_policies`, key `llm.providers.<name>.retry`, TC-RETRY-8..13). Until now one policy served every route, so a document could not say that one provider rate-limits and another times out. A block is the whole policy for its route rather than a patch on the general one, because layering the two would hand `mode: always` a `max_retries` its author never wrote - the rule TC-RETRY-5 refuses. Installing a resolved block on that route is the next slice.
