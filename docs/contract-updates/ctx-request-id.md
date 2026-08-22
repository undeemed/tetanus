# Contract note: the provider's request id on a refusal

For folding into [`../interface-contract.md`](../interface-contract.md) by the
reconciliation slice. Written here rather than in place because every lane
collides on that file, and because AGENTS.md says a contract change lands as
its own commit touching the document and the code it publishes, never inside a
feature branch.

Branch: `fm/tetanus-p5-request-id`.
Code: `crates/engine/src/convert.rs` (§4.5) and
`crates/turn/src/llm/retry.rs` (§4.3.2). The engine-internal half - the field
on `LlmError::Provider`, the header rule, `RequestFailure` - is the commit
before this one and proposes nothing here: it crosses no boundary.

---

## 1. What changed

Two published payloads gain one optional field each, carrying the id the
provider gave the request it refused.

Nothing is removed, no type is renamed, no error code is added and no Rust
type in `crates/protocol` changes: both payloads are `serde_json` objects on
the wire (`RpcError.data` and `SessionEvent.data`), which §5 makes a minor
change rather than a build break. A client that ignores the field reads
exactly what it read before.

## 2. Why it is worth a contract change at all

§4.5 already says the thing that decides this: **`message` is a plain sentence
for a log, and the presentation lane may replace it with its own wording, keyed
on the code.** A fact carried only in the message is therefore a fact a
conforming surface is entitled to delete. The request id is the one fact about
a refusal nobody can reconstruct - the status is on the response, the words are
in the body, the classification is a function of both, and the id exists only
in the provider's own logs - so it is precisely the fact that must not live
somewhere a surface may drop.

It is also the only thing a person can quote to a provider's support. A harness
that discards it makes "my request failed and nobody can tell me why"
unanswerable by anyone, which is the state tetanus was in until this change.

The wait a throttled provider asks for is deliberately **not** published beside
it, and the reason is the test of the rule above: a surface can render a wait
usefully from what it already has ("retrying"), and the engine acts on it
before any surface sees it. Nothing can render an id it was never given.

## 3. Proposed edit to §4.5

The `ProviderError` row of the error table:

| Code | Name | `data` | Exit status |
| --- | --- | --- | --- |
| -32006 | `ProviderError` | `{ provider, status }`, plus `request_id` when the provider named one | 6 |

And after the paragraph that fixes the mapping as the engine's:

> `request_id` is the provider's own identifier for the request it refused,
> present only when the response carried one. It is `data` rather than part of
> the message because this section lets a surface replace the message, and an
> id a surface may delete is an id a user cannot quote. A refusal that never
> reached a provider - a transport or protocol failure - carries no id and no
> key, for the reason `status` is absent there: an absent key says the fact
> does not exist, where a null invites a surface to print one.

## 4. Proposed edit to §4.3.2

The `llm/retry` row of the durable payload table:

| `type` | `data` |
| --- | --- |
| `llm/retry` | `turn`, `step`, `provider`, `code`, `message`, `request_id` (`null` when the provider named none), `retry`, `max_retries` (`null` under an unbounded policy), `delay_ms` |

And after the sentence that says what `code` and `message` are:

> `request_id` is the provider's id for the attempt that failed. It is on the
> record because a retried refusal is reported to nobody: the turn recovered,
> no error was ever returned, and the journal is then the only place that can
> answer which requests a provider refused and under what ids.

Note the two spellings differ on purpose. The durable record carries `null`,
because §4.3.2's own `max_retries` already sets that convention for a fact that
does not apply, and a journal reader folding records wants a uniform shape. The
error object omits the key, because §4.5's `status` already sets *that*
convention and a surface reading `data` is rendering one failure rather than
folding many.

## 5. Compatibility

- **Wire**: additive, minor by §5. No `PROTOCOL_VERSION` bump is proposed;
  neither payload is a Rust type a lane constructs or destructures.
- **`KnownEvent`**: unaffected. `llm/retry` is one of §4.3.2's staged types
  with no variant, so a surface still renders it raw.
- **The rest-pattern rule**: not engaged. Nothing here adds a field to a struct
  the presentation lane matches.
- **What the presentation lane may now do, and this note does not ask it to**:
  render the id beside a provider failure so a user can copy it. That is that
  lane's change, in `crates/cli/src/render/fault.rs`, whenever it wants it.

## 6. Cases

| Case | Where | What it holds |
| --- | --- | --- |
| TC-REQID-6 | `crates/engine/tests/faults.rs` | the published error carries `request_id` when the provider named one, and no key at all when it did not or when the call never reached a provider |
| TC-PORT-REQID-5 | `crates/turn/tests/upstream_retry_executor.rs` | the `llm/retry` record read back off the file carries the id of the attempt it describes |
| TC-PORT-RETRYX-1 | same | the same record carries `null` when the provider named none, rather than omitting the key |

## 7. Changelog row

| 1.0 | Publishes the provider's own id for a refused request in the two places a refusal is reported (§4.5, §4.3.2): `request_id` on `ProviderError.data` when the provider named one, and on the `llm/retry` record for a refusal a policy recovered from. Additive and minor by §5 - both are `serde_json` payloads, no Rust type in `crates/protocol` changes, no code is added and nothing is renamed. The id is `data` and not part of the message because §4.5 already lets a surface replace the message with its own wording keyed on the code, so a fact carried in the sentence is a fact a conforming surface may delete - and this is the one fact about a refusal nobody can reconstruct, since the status is on the response and the words are in the body but the id exists only in the provider's logs. It is also the only thing a user can quote to a provider's support, which the harness discarded until now. The wait a throttled provider asks for stays unpublished, and the contrast is the argument: a surface can say "retrying" from what it already has. Two spellings on purpose - the durable record carries `null` for a provider that named none, following `max_retries`, and the error object omits the key, following `status`, because one is folded by a reader of many records and the other is rendered as one failure. The journal half exists for the case nobody is watching: a retried refusal is reported to no one, so the record is the only thing left that can say what the provider refused. Rendering the id is the presentation lane's change and is not asked for here. |
