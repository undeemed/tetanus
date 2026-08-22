# Contract note: context and durable state

For folding into [`../interface-contract.md`](../interface-contract.md) by the
reconciliation slice. Written here rather than in place because every lane
collides on that file, and because AGENTS.md says a contract change lands as its
own PR touching both the document and `crates/protocol`, never inside a feature
branch.

Branch: `fm/tetanus-p2-ctx`.

---

## 1. What changed, and why it is not a breaking change

This branch adds five durable event types. It changes no boundary struct, no
method, no error code and no existing payload.

Section 4.3.2 is what makes that safe, in its own words: `type` stays a free
string because the durable vocabulary grows, and a surface must pass an unknown
type through rather than drop it; the `KnownEvent` variant follows later.

So these five are additive by the mechanism the contract already published. A
surface built against today's document renders them raw and loses nothing. The
only edits proposed here are to the *list* in §4.3.2 and the table in §4.3.1,
both of which are descriptions of what exists rather than promises about what
cannot.

## 2. Proposed edit to §4.3.2

The sentence naming the vocabulary a surface renders today currently reads:

> The durable vocabulary a surface renders today: `session/start`,
> `turn/start`, `step/start`, `user/message`, `assistant/chunk`,
> `assistant/message`, `tool/call`, `tool/result`, `step/end`, `turn/end`.

Five types are now written that it does not name. Suggested addition after that
sentence, rather than an extension of the list itself - the ten are the ones a
surface is expected to render specially, and these five are ones it is expected
to pass through:

> Five more are written and are not in that list, because a surface needs
> nothing special to render them: `request/context`, and the four
> `compaction/*` types below. They are the worked example of the rule this
> section states - a build that has not learned them shows them raw, and
> `KnownEvent` has no variant for any of them yet.

## 3. Proposed additions to the §4.3.1 table

| `type` | `data` |
| --- | --- |
| `request/context` | `turn`, `step`, `provider`, `model`, `context_window`, `system_tokens`, `tools_tokens` |
| `compaction/start` | `shadowed_range` |
| `compaction/summary` | `start_seq`, `summary`, `provider`, `model`, `shadowed_range`, `shadowed_seqs`, `shadowed_token_count` |
| `compaction/end` | `start_seq`, plus `error` on an attempt that did not commit |
| `compaction/prune` | `shadowed_range`, `shadowed_seqs`, `shadowed_token_count` |

## 4. The two rules a reader of the journal needs

Both belong somewhere near §4.4.2, which is where the turn's own event order
lives. Wording is a suggestion; the facts are what matter.

### 4.1 `request/context` is written before the request, not after the answer

A step writes it after the system prompt is assembled - because it prices that
assembly - and before `agent/request` - because a listener that rewrites the
request must not be able to change what the journal already said the request
was. It is written before the provider is called at all, so a turn that a
provider failure ended still says what it tried to send.

It is the anchor the context projections fold
(`crates/turn/src/projections.rs`). Before it existed, `docs/parity.md` named
the missing request envelope as the blocker on three token projections, in
three separate rows.

### 4.2 A compaction record and its replacement are adjacent, and that is contractual

`compaction/summary` and `compaction/prune` each name a range of surface events
and state that range's heuristic price. **The next surface event on the log is
the replacement for that range.** Nothing may be appended between them.

This is not a tidiness rule. It is what lets a consumer with bounded state -
one running total and at most one pending claim - price a replacement without
retaining a price per message, which is the difference between a projection
checkpoint that stays a fixed handful of numbers and one that grows for the
life of a session.

Two consequences a reader can rely on:

- A record followed by anything other than a surface event shadows nothing. It
  described a replacement that never landed, and honouring it against a later
  event would shadow a range that event never named.
- A replacement arriving with no adjacent record folds neutrally rather than
  failing. A journal written before this protocol existed has no record to
  find, and bounded state cannot reconstruct what was replaced; degrading to
  drift keeps replay working, where refusing would make old journals
  unreadable.

The replacement takes the **position** of the range it shadows, not the end of
the conversation. A checkpoint condensing the first twenty messages belongs
where those twenty were, in front of the tail that was kept verbatim.

## 5. Section 4.7: one new settings key

`sessions.backend` selects the session-persistence artifact: `jsonl` (the
default, one journal file per session) or `sqlite` (one database under the
sessions root, holding every session). It is resolved at boot, and both an
unserved name and a database this build cannot open are refused there rather
than at the first `session.create`.

Under the `sqlite` backend, `session.create` with a `path` is `InvalidParams`
naming that field: a session inside a database is named by its id, and
answering a caller's named file with some other session's log would be worse
than refusing.

`config.dump` reports `sessions.backend` like any other settled key.

## 6. What is deliberately *not* proposed

`assistant/message` still carries no `turn` or `step`, and this branch does not
add them. The usage projection wanted them and gets the same fact from the
`step/start` that encloses the report, which needs no contract change and is
the more robust rule - it works for a hand-built journal too. Adding two fields
to a published payload would be a contract change carrying no behaviour, which
is the kind this document's own process exists to keep out of a feature branch.
