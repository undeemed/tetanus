# Parity note: a refused answer's provider request id

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.
Written here rather than in place because every branch in flight collides on
that file.

Branch: `fm/tetanus-p5-request-id`.
Scope: the one gap the previous sweep of this lane's areas left open with a
measurement rather than a guess (`sweep-unclaimed-gaps-in-context-areas.md`,
§2.1). It is closed.

---

## 1. What was built

Upstream keeps a `requestId` on its `LlmFailure`
(`packages/llm/llm/src/types.ts`), reads it off a refusal in the DeepSeek
adapter, and carries it as far as the durable retry record its invariant
checker validates. tetanus read the same response for the wait it asked for,
took the body, and threw the id away with the response.

Now: `LlmError::Provider` carries a `request_id`, read through an accessor
beside `retry_after_ms`; the production transport fills it from the response
headers; `RequestFailure` carries it to `agent/request-error`; the `llm/retry`
record carries it onto the journal; and the published `ProviderError` carries
it in `data`. The contract half is
[`../contract-updates/ctx-request-id.md`](../contract-updates/ctx-request-id.md).

Cases: TC-PORT-REQID-1..5 restate upstream's, TC-REQID-1..6 pin the tetanus
rule. They live where the thing they test lives -
`crates/turn/tests/upstream_request_id.rs` for the header rule and the accessor,
`crates/turn/tests/deepseek_adapter.rs` for the two that read a real response
off a loopback endpoint, `crates/turn/tests/upstream_retry_executor.rs` for the
journal record, `crates/engine/tests/faults.rs` for what the boundary
publishes.

## 2. Row edits, section 3

The `llm/*` row. Section 3 currently holds **three** union-merged copies of it,
and all three name this gap; every copy needs the edit, or the collapse the
previous sweep asked for needs to happen first.

**Gap**: remove `a refused answer's provider request id` (in one copy it reads
`a refused answer's provider request id and its terminal-quota-versus-throttle
distinction`, where only the first clause goes - the quota half was closed
separately and that copy is simply stale).

**Today**: add `the provider's own id for a refusal, read off the response and
carried to the recovery point, the journal and the published error`.

## 3. Row edits, section 4

| Row | Edit |
| --- | --- |
| `llm/llm-deepseek/tests/adapter.spec.ts` | The sentence ending "The provider request id, the context-window and terminal-quota classifications and the thinking-mode configuration have no surface, so those cases have nothing to restate" now names one fewer: drop the request id from the list and add "TC-PORT-REQID-1 and -2 restate the two cases that read an id off a refusal, including this provider's own spelling of the header." |
| `llm/llm/src/error.ts` (`isQuotaExceededError`, ...) | The row ends "The provider request id remains a gap". Replace with "The provider request id is served (TC-PORT-REQID-1..5); TC-REQID-5 pins that carrying it changes no classification, in either direction." |
| `llm/llm-retry/tests/invariant.spec.ts` | The row says upstream's payload refusals have nothing to restate because the executor builds the record out of typed values. Still true, and one clause now has a positive restatement instead: add "Upstream's `failure.requestId` clause is restated as behaviour rather than as a refusal - TC-PORT-REQID-5 asserts the id is on the record, and the header reader never produces an empty one (TC-REQID-3), which is the condition upstream validates for." |
| `llm/llm/tests/service.spec.ts` | Has no row of its own; its structured-facts case is restated as TC-PORT-REQID-3 in `crates/turn/tests/upstream_request_id.rs`. Worth a row if the reconciliation slice is adding any. |

## 4. What is left in this area

Nothing about the request id. The remaining `llm/*` gaps are unchanged by this
work: further providers, a measurement anchored on real provider usage, and the
three token projections.

One thing this touched but did not close, named so it is not mistaken for
closed: **no surface renders the id yet.** It is in `ProviderError.data` where
a surface can reach it, and `crates/cli` is the presentation lane's by the
contract's file-ownership table, so the rendering is that lane's change. The
engine's promise is that the fact survives to them.

## 5. Changelog row

| 2026-08-22 | A refused answer's provider request id, which the previous sweep of this lane's areas measured and left open (`crates/turn/src/llm/mod.rs`, TC-PORT-REQID-1..5 and TC-REQID-1..6). It is the one fact about a refusal a harness cannot reconstruct from what arrived - the status is on the response, the words are in the body, the classification is a function of both, and the id exists only in the provider's own logs - and it is the only thing a person can quote to a provider's support. tetanus read those same headers for the wait a throttled provider asks for, took the body, and dropped the id with the response. The header rule is a list rather than a name because there is no standard header; `cf-ray` is in it, last, because a refusal generated at a CDN edge never reached the provider and carries none of the others, and a blank header is skipped rather than taken as the answer, which is the same rule upstream enforces by refusing an empty id at construction. The id travels three ways for three different readers: to a recovery listener on `RequestFailure`, onto the `llm/retry` record for the case nobody is watching - a refusal a policy recovered from is reported to no one, so the journal is the only thing left that can say what was refused - and into the published error's `data` for the refusal that ends a turn. `data` and not the message, because section 4.5 lets a surface replace the message with its own wording keyed on the code, so a fact in the sentence is a fact a conforming surface may delete. The wait stays unpublished and the contrast is the argument: a surface can render "retrying" from what it already has, and nothing can render an id it was never given. One case exists because a new field on an error is a chance to change what that error means by accident: TC-REQID-5 holds the classification identical with and without an id, across every status whose code differs. Rendering it is the presentation lane's, in `crates/cli`, and is deliberately not here. |
