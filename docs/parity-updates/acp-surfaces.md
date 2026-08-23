# Parity note: external contract surfaces

Lane: `acp/*`, `sdk/*`, `api/*`, and the query half of `session-query/*`.
Branch: `fm/tetanus-p3-acp-client`, which carries `fm/tetanus-p3-acp`'s four slices rebased onto
`master` plus the client half of the bridge.

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
| `acp/*`, `sdk/*`, `api/*` | 17 | Own JSON-RPC contract in `crates/protocol` and both carriers; an ACP bridge riding that carrier - initialize, session/new, session/prompt, one-way cancel, committed assistant messages and tool calls as `session/update`, one-shot fail-closed permission requests, and `session/load`, which re-opens a session by replaying its journal as `session/update` frames before it answers, advertised as `loadSession` and usable afterwards rather than read-only - and the client half that spawns an agent process and drives it over real pipes, answering its permission questions under a policy, demultiplexing one stream, bounding every wait and reaping the child; an in-process SDK client and owned-run API that drives a whole turn with no CLI and no socket, enforcing the handshake exactly as a carrier does and closing its own subscriptions; the request surface as an enumerable descriptor catalog validating exact named arguments before dispatch | Image and audio prompts, which wait on `tetanus_turn::llm::Message::content` carrying parts rather than a `String` - not on storage, which `crates/features` now has - session list and fork, which the ACP revision this speaks has no call for at all, editor navigation, modes, plans and titles, upstream's generated Typert codecs and its client-side remote projection, an approval seam the engine can drive the bridge's permission channel from | ③ for the rest |

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

`TC-PORT-ACP-1..31`, `TC-PORT-SDK-1..12`, `TC-PORT-API-1..14`, `TC-PORT-QUERY-1..19`.
All offline; none needs a key, a network, or the binary.

`TC-PORT-ACP-17..24` are the client half, and every one of them spawns a second process: the test
binary re-entered as the agent, serving real ACP on its own stdin and stdout, the same self-re-entry
`crates/ui/tests/killed.rs` uses. Frames cross an operating-system pipe rather than a `duplex`,
because the failures worth catching here - an unanswered `session/request_permission`, a child that
stops speaking, frames interleaved on one stream - are not reachable with a double on the other end.
Every wait in them carries a deadline for the same reason.

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
   every other answer denying - is ported and pinned by TC-PORT-ACP-13, and the client's side of it
   by TC-PORT-ACP-21 under both policies. What is missing is an engine-side approval seam for the
   bridge to hang it on; `Engine` exposes none today, and adding one is a contract change rather
   than a lane change.
7. **The client refuses by default.** Upstream's driver is configured per call site. Here
   `PermissionPolicy` defaults to `Reject`, because the safe answer to "I did not think about this"
   is a denied tool call rather than a command that has already run, and it only ever selects an
   option the agent actually offered - answering with an id the agent never listed would be making
   up protocol.

## A defect the client's own deadline caught

Worth recording because it is the shape of failure this half exists to find. `AcpClient::close`
shut the write handle down while the client still owned it. A pipe closes when its writer is
*dropped*, so the descriptor stayed open, the child never reached end of file, and every teardown
waited out the ten-second kill fallback and then killed a process that would have exited on its own.
It passed either way - the child did end, the assertion was about the child being gone - and only a
clock could tell the graceful path from the violent one. TC-PORT-ACP-22 bounds the close for exactly
that reason, and the handle is now taken out of its `Option` and dropped.

## A hunk this branch carried deliberately, and no longer carries

Resolved as designed, and kept here because the design is the point.

`crates/ui/tests/killed.rs` carried a one-line change plus its comment, copied **verbatim** from
`fm/tetanus-p2-mcp` at `779ae80`, where the mcp lane fixed it first - byte-identical to that
commit's version, same blob, `3e2784b..480d093`. The mcp lane landed first, so on the rebase onto
`e1fc356` git recognised the patch as already upstream and dropped it. This branch now contributes
nothing to that file, which is exactly what the last line of this section predicted; had the two
spellings differed, the rebase would have raised a conflict over one decision written twice.

The duplication was intended, not an accident of two lanes racing. Both branches had to be
independently green under the fleet's `RUST_TEST_THREADS=1`, and TC-UI-TERM-5 failed under that
cap for a reason neither lane caused: with one test thread libtest writes `test <name> ... `
with no trailing newline before the case runs, so the child's `armed` marker lands on the end of
that line and the parent's exact-equality match never fires. Because the two hunks are
identical, the fold reads them as one change and resolves without a conflict; had this lane
written its own wording for the same fix, the fold would have had to adjudicate two spellings of
one decision.

Whichever branch lands second contributes nothing here. That is the intended outcome.

## What is left in the row, and precisely what each waits on

The row's gap column was three words - "ACP bridge, SDK client, gateway" - and all three are
served. What remains is written as clauses rather than as a list, because a gap nobody can name the
blocker for is a gap the next sweep re-derives from scratch.

1. **Image and audio prompts.** *Not* waiting on storage any more, and the earlier note in this file
   saying so was overtaken: `crates/features` has had a content-addressed attachment store since the
   feature-tools slice landed. The actual blocker is one field. The model-visible message this
   workspace sends is `tetanus_turn::llm::Message`, whose `content` is a `String`; until that seam
   carries parts rather than a string, an admitted image has nowhere to go in the request, and
   storing the bytes would be keeping something no turn can refer to. `PromptCapabilities` therefore
   still advertises `image: false`, which is the honest answer and the one a client can adapt to.
   Audio is further back still: no adapter here speaks it.

2. **The permission channel is built, pinned on both sides, and still not driven by a turn.**
   `AcpBridge::request_permission` maps ACP's two one-shot options onto `ApprovalOutcome` and every
   other answer onto a denial (TC-PORT-ACP-13, and the client's half in TC-PORT-ACP-21). What is
   missing is the engine-side seam that would raise the question: `EventSink` carries exactly two
   pushes, `session_event` and `agent_status`, and neither is a *request* the client answers.
   `tetanus_protocol::methods::push` already names `ui/ask` and `ui/approve` for this, so the
   vocabulary is agreed and the carrier is not: adding a server-to-client request to `EventSink` is
   a change to the published boundary, which is its own pull request touching
   `docs/interface-contract.md` and `crates/protocol` together, never a line inside this lane.

3. **`agent.steer` is declared, routed, capability-named, reserved - and missing from
   `method::ALL`.** Still true on this master. It is one line in an engine-lane file, so it stays a
   proposal (`docs/contract-updates/acp-gateway.md`) and TC-PORT-API-3 pins the omission so that
   fixing it upstream of this lane cannot go unnoticed.

4. **Session fork and session list are not ACP gaps.** The revision this bridge speaks has exactly
   two ways to obtain a session - `session/new` and `session/load` - and no call for forking one or
   for listing what exists. The engine serves both (`session.fork`, `session.list`) and
   `tetanus_sdk::Client` exposes them typed, so the capability is present and it is ACP that has
   nowhere to put it. Inventing `session/fork` here would be this bridge extending someone else's
   protocol, which is the one thing a bridge must not do.

5. **Editor navigation, modes, plans and titles** stay out for the same reason and a second one:
   each is a surface concern whose tetanus half is owned by the presentation lane, and `crates/acp`
   holds no engine type and writes no user-facing copy.

## A probe that stopped being a probe

Worth recording because it is the same trap `AGENTS.md` already names for `TC-ENG-4`/`TC-RPC-12`.
TC-PORT-ACP-16 asserted that an unknown method is refused rather than ignored, and the method it
used to prove it was `session/load` - which this slice now serves. The case would have gone on
passing right up until it did not, and then failed for a reason that had nothing to do with what it
was written to check. It now probes with a method ACP does not define at all, and says in the case
why that distinction matters.

## Related proposals

- `docs/contract-updates/acp-gateway.md` - `method::ALL` omits `agent.steer`, which is
  declared, routed, capability-named and reserved. Pinned by TC-PORT-API-3 so that fixing it
  in the owning lane does not go unnoticed.
- `docs/contract-updates/acp-bridge.md` - the additive `FrameSink`/`FrameHandler`/
  `serve_handler` seam this lane added to `crates/rpc`, which is an engine-lane file.
