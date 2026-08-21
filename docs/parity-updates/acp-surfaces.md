# Parity note: external contract surfaces

Lane: `acp/*`, `sdk/*`, `api/*`, and the query half of `session-query/*`.
Branch: `fm/tetanus-p3-acp`.

This file is this lane's input to `docs/parity.md` section 3. It is not an edit to that
file - the lane does not edit shared docs - it is the two rows the lane's work changes,
written so whoever reconciles them can paste rather than reconstruct.

## Rows this lane changes

### `acp/*`, `sdk/*`, `api/*`

Before:

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `acp/*`, `sdk/*`, `api/*` | 17 | Own JSON-RPC contract in `crates/protocol`, carriers in progress | ACP bridge, SDK client, gateway | ③ |

After:

| Upstream area | Specs | Today | Gap | Closes in |
| --- | ---: | --- | --- | --- |
| `acp/*`, `sdk/*`, `api/*` | 17 | Own JSON-RPC contract in `crates/protocol` and both carriers; an ACP bridge riding that carrier - initialize, session/new, session/prompt, one-way cancel, committed assistant messages and tool calls as `session/update`, one-shot fail-closed permission requests; an in-process SDK client and owned-run API that drives a whole turn with no CLI and no socket, enforcing the handshake exactly as a carrier does and closing its own subscriptions; the request surface as an enumerable descriptor catalog validating exact named arguments before dispatch | Image and audio prompts (they need the durable attachment store phase ② brings), session load/list/resume/fork over ACP, editor navigation, modes, plans and titles, upstream's generated Typert codecs and its client-side remote projection, an approval seam the engine can drive the bridge's permission channel from | ③ for the rest |

### `session/*`, `session-query/*`

Only the query half moves; the persistence half is another lane's. The `Today` column
gains, and the `Gap` column loses, the query clause:

- **`Today` gains:** reading the journal as data - a one-pass fold that derives each event's
  turn and step from the structural events that carry them, filtering by turn, step, role,
  tool, time, seq, outcome and literal text with clauses ANDed and values ORed, paging by seq,
  and three named aggregates: every tool call paired to its result by `call_id`, every turn a
  named tool failed in, and what a range of turns cost with unpriced messages counted rather
  than silently treated as free.
- **`Gap` loses:** "log export and query" becomes "log export". Still absent: full-text search
  and its cursors, SQLite-backed indexing, lineage and event tracing, and title snapshots.
- **`Closes in`** becomes `② for persistence` alone; the query clause is served.

## Cases

`TC-PORT-ACP-1..16`, `TC-PORT-SDK-1..12`, `TC-PORT-API-1..14`, `TC-PORT-QUERY-1..19`.
All offline; none needs a key, a network, or the binary.

## Departures from upstream worth recording

These are places the restatement deliberately differs, rather than places it falls short.

1. **Tool activity is on the ACP wire.** Upstream's bridge is automation-only and keeps tool
   calls off it. ACP has first-class `tool_call` and `tool_call_update` variants, and
   `docs/interface-contract.md` §7.2 already fixes that the journal *is* the stream, so
   withholding them would build the second, quieter history §7.2 exists to reject.
2. **`max-steps` maps to ACP `max_turn_requests`, not `max_tokens`.** What ran out is the
   driver's budget of model requests. A client told `max_tokens` would retry with a shorter
   prompt and be wrong about why.
3. **A harness rejection maps to `end_turn`, not `refusal`.** ACP's `refusal` means the model
   declined; reporting a harness decision as one puts words in the model's mouth.
4. **The ACP session id is the engine's own.** Upstream mints a branded id and keys a table by
   it. One id means an operator holding an ACP session id can find its journal, and means
   there is no mapping table to fall out of step.
5. **The gateway holds no handshake.** Upstream's gateway is reached through a Connection that
   owns that state. Here the handshake is per connection, a gateway is not one, and
   `tetanus_sdk::Client` is where connection state lives. Two components each half-enforcing
   the rule would be the worst of both.
6. **The permission channel is built and not yet driven.** The mapping - two one-shot options,
   every other answer denying - is ported and pinned by TC-PORT-ACP-13. What is missing is an
   engine-side approval seam for the bridge to hang it on; `Engine` exposes none today, and
   adding one is a contract change rather than a lane change.

## A hunk this branch carries deliberately

`crates/ui/tests/killed.rs` carries a one-line change plus its comment, copied **verbatim** from
`fm/tetanus-p2-mcp` at `779ae80`, where the mcp lane fixed it first. It is byte-identical to that
commit's version - same blob, `3e2784b..480d093`.

The duplication is intended, not an accident of two lanes racing. Both branches must be
independently green under the fleet's `RUST_TEST_THREADS=1`, and TC-UI-TERM-5 fails under that
cap for a reason neither lane caused: with one test thread libtest writes `test <name> ... `
with no trailing newline before the case runs, so the child's `armed` marker lands on the end of
that line and the parent's exact-equality match never fires. Because the two hunks are
identical, the fold reads them as one change and resolves without a conflict; had this lane
written its own wording for the same fix, the fold would have had to adjudicate two spellings of
one decision.

Whichever branch lands second contributes nothing here. That is the intended outcome.

## Related proposals

- `docs/contract-updates/acp-gateway.md` - `method::ALL` omits `agent.steer`, which is
  declared, routed, capability-named and reserved. Pinned by TC-PORT-API-3 so that fixing it
  in the owning lane does not go unnoticed.
- `docs/contract-updates/acp-bridge.md` - the additive `FrameSink`/`FrameHandler`/
  `serve_handler` seam this lane added to `crates/rpc`, which is an engine-lane file.
