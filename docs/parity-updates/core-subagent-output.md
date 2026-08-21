# Parity update — a child's final answer

Slice: `core-subagent-output`. Lane: engine/core (phase ②, subagent block).

## Section 4 — ported

| Upstream | Ported to | Cases |
| --- | --- | --- |
| `packages/subagent/subagent/src/assistant-output.ts`, `tests/assistant-output.spec.ts` | `crates/subagent/src/assistant_output.rs`, `crates/subagent/tests/assistant_output.rs` | TC-SUB-OUT-1..10 |

## Adapted to this journal's vocabulary

Upstream folds `ContentBlock[]`. In this workspace an `assistant/message`
carries its content as a **string** and an `assistant/chunk` is a tagged stream
chunk (`{"chunk":"text","delta":…}`), so the fold reads those. The selection
rule is unchanged; only what it reads is.

That also means the reasoning channel is visible to the fold and must be
excluded, which upstream never has to say — the mutation check confirms it
bites.

## Cases beyond the upstream suite

- **TC-SUB-OUT-8** — the fold can be read repeatedly while it grows. A backend
  watching a live child asks more than once, and a `collect` that consumed the
  fold would answer differently the second time.
- **TC-SUB-OUT-9** — text from outside the journal joins the *same* fallback.
  A transport carrying content without journal records has no
  `assistant/chunk`, and two separate buffers would not concatenate into one
  answer.
- **TC-SUB-OUT-10** — a malformed record is ignored, not fatal. These are read
  back off disk where a truncated write or an older writer leaves a field
  missing or wrong-typed, and a fold that panicked would fail the parent's run
  over the child's journal.

## Named equivalent mutant

`push_text` guards against an empty piece, and **no case can distinguish that
guard**, because `collect` already treats an empty fallback as no answer.
Pushing `""` leaves the buffer empty either way. The guard is kept because it
states the intent and avoids pointless work, not because it changes behaviour;
recorded here so a future reader does not mistake the surviving mutant for a
gap in the suite. Four of the five mutations tried are caught.

## Changelog row

| 2026-08-21 | A child's final answer ported (`crates/subagent/src/assistant_output.rs`, TC-SUB-OUT-1..10). When a delegated run ends the parent needs one thing back, and a journal holds several candidates; picking wrong is how a parent reports an empty result for a run that plainly said something. The rule is the last non-empty assistant message, else the accumulated streamed text, else nothing. Two details carry it: *last*, because a child that kept working after an intermediate answer meant the later one, and *non-empty*, because the loop appends an empty message to record usage after a step with no visible output and letting that win would erase a real answer. Selection ignores the stop reason - an interrupted child still said what it said. Adapted to this journal's vocabulary, where a message's content is a string and a chunk is tagged, which additionally means the reasoning channel is visible to the fold and must be excluded. Three cases go beyond upstream, all about a live or damaged read: the fold answers the same way when asked twice, text arriving outside the journal joins the same fallback, and a malformed record is ignored rather than failing the parent's run. One equivalent mutant is named rather than papered over: the empty-piece guard in `push_text` cannot be distinguished by any case, because `collect` already treats an empty fallback as no answer. |
