---
date: 2026-08-20
order: 41
---
A turn the provider's output cap cut off ends at the cap (`StopReason::MaxTokens`, `ModelResponse::truncated`, TC-PORT-CAP-1..5, closing upstream's max-tokens cases in section 4). Until now `finish_reason` was journalled and read by nothing, so a completion that stopped mid-sentence ended `natural` - a reader was told the model had finished - and the half-written calls it carried were dispatched with arguments nobody could know were complete. It found a second defect on the way: dropping the dispatch is not enough, because the calls stayed on the `assistant/message`, so the derived history asked the provider for a `tool/result` that would never come and the *next* request on that session would have been refused. The anchor now carries the text the model did write, the provider's own finish reason and no calls (`docs/interface-contract.md` §4.4.2).
