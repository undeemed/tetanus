# Parity: the run's path

Upstream: [`client/ui-trajectory`] - "the run's path - steps, retries,
timings", 12 components.

tetanus: `web/app/trajectory.js`, opened from the header.

## Why it is a module of its own

The thread answers "what was said"; the trace answers "what actually
happened". They are different questions and they want different shapes: a
conversation reads top to bottom in prose, a trace reads as a table of steps
and durations. Upstream separates them and so does this.

## Built against what this build writes

Every figure comes off the journal this tree already produces - `turn/start`,
`step/start`, `assistant/chunk`, `assistant/message` with its `usage`,
`tool/call`, `tool/result`, `step/end`, `turn/end`, and the time on each. The
arithmetic is deliberately the same as the terminal's closing line and
`/stats`: two surfaces disagreeing about how long a turn took would be worse
than neither showing it.

Two durations are kept apart rather than summed, because they are different
problems: the **wait for a first token** is the provider, and the **decoding**
after it is the answer's length. One number cannot tell a slow provider from a
long reply.

A tool's time comes from **the call its result names** (§4.3.1), never from the
call before it - two calls in flight finish in whichever order the tools
finish, which is why the contract pairs them by id.

## Registered, and empty until an engine writes them

Per the standing instruction: build the frame, register the view, do not fake
the data.

| Event | Status here | Whose work |
| --- | --- | --- |
| `llm/retry`, `llm/retry-started` | folded and drawn; no emitter in this tree | the retry policy lane |
| `context/snapshot` | folded and drawn; no emitter in this tree | the context lane |

Both are durable types in §4.3.2 of the contract *on this branch*, so their
shapes are published rather than guessed. The rows are wired by event type, so
the day an engine writes one it appears, and until then the trace simply has
none - no placeholder, no mock row.

For the record, the projections the gap list mentions do not exist anywhere
yet: `fm/tetanus-contract-runtime-context` touches only `crates/protocol/tests/wire.rs`
and the contract document, and nothing in this repository defines a projection
surface. A trajectory built on projections would have been built on nothing.

## Tests

`target/probe-primitives.mjs`, **54/54**: the fold finds turns and steps, keeps
the first-token wait apart from the decoding, credits each tool from the call
its result names - including two in flight finishing out of order - folds a
retry and a context snapshot when they are written, draws the whole thing, says
so when nothing has run, and reads durations as milliseconds below a second and
seconds above.

Verified in Chrome against a live turn: `turn 1 · 33ms · natural`, `step 1 ·
first token 7ms · decoding 6ms · 5 out · echo 6ms`, `step 2 · first token 1ms ·
decoding 2ms · 4 out`. Screenshot at `data/tetanus-ui-handoff/webui-trace.png`.
