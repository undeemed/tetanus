# Parity note: the runtime context a turn tells the model

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.
Written here rather than in place because every branch in flight collides on
that file.

Branch: `fm/tetanus-p6-ctx-rows`.
Scope: the `context/*` row's "Time ... context", and the producer contract
section 4.4.8 has been waiting for since it was published.

---

## 1. What was built

`crates/turn/src/context.rs`: a registry of named runtime-context providers,
gathered once per turn between `turn/start` and the first `step/start`,
recorded as `context/snapshot`, and derived into one user message.

This closes a published-with-no-producer gap of exactly the kind this lane has
now found three times. Section 4.4.8 is a page of the interface contract,
`context/snapshot` is in the section 4.3.2 table, and TC-PROTO-25 has asserted
its staging at the boundary since before anything wrote one - **no journal
tetanus had ever written carried a snapshot**, and `crates/turn/src/prompt.rs`
said so in its own module docs ("what upstream also keeps there and tetanus has
no surface for - scopes and runtime-context providers - stays a row in
`docs/parity.md`").

Cases: TC-PORT-RTCTX-1 restates upstream's time reading; TC-RTCTX-2..10 pin the
contract's rules - the joining rule, the deployment's ordering, one snapshot per
turn before the first step, a silent deployment writing nothing, only the newest
travelling, where it sits in the history, a replay deriving the same reading,
the gather happening once, and the two clock edges the one hand-written date
conversion in the workspace has to survive.

## 2. Two decisions that differ from upstream, and why

**No display time zone.** Upstream renders in a zone resolved from
configuration, the process, or a zone it derives from the browser; tetanus
reports UTC and says so in the text. A display zone means a time-zone database,
and the workspace has none - the same reason `crates/turn` matches phrases
where upstream matches regular expressions. A reading nobody can misread beats
a local time this build cannot be sure it converted.

**One record, not one message per provider.** Upstream builds each context as
its own `agent/pre-step` plugin appending its own user message. tetanus gathers
them into one `context/snapshot`, because section 4.4.8's rule that only the
newest travels needs a single record to be decidable.

One further reading of the contract is worth recording, because it was found by
a case rather than by argument. Section 4.4.8 says the snapshot is carried
"after the retained history", which reads as "last" - and upstream does append
last. tetanus puts it where the journal puts it: after everything the turn
inherited, immediately before the message that opened the turn. Both satisfy
the caching property the clause exists for. The journal's order also keeps a
request ending in the user's ask or a tool's result, and the thing that reads
the last message as the thing to answer is not only a model: this crate's own
mock adapter does, and with the block last it answered the block and the turn
never settled (TC-RTCTX-7 carries the reasoning).

## 3. Row edits, section 3

**`context/*`, `guard/*`.** Gap: remove `Time and tmux context`, leaving
`re-rendering an instruction file a tool edited mid-session, timeout and repeat
guards` - and see section 4 below for what tmux now needs, which is not this.
Today: add `the runtime context a turn tells the model - named providers
gathered once per turn, recorded as context/snapshot, carried as a user message
so the cached prompt prefix is not disturbed, with only the newest one
travelling`.

## 4. What is left in this row, and what it waits on

- **tmux context.** Upstream reads a pane's title and running command from a
  live `tmux` server. The provider seam is now here and it is a dozen lines
  over `crates/exec`; what it waits on is a decision, not code - whether a
  harness shells out to a program that may not be installed in order to
  describe a terminal the model cannot see. Worth its own slice with that
  question answered first, rather than a provider nobody asked for.
- **Re-rendering an instruction file a tool edited mid-session.** Unchanged by
  this work and genuinely open. It is a prompt-section concern, not a context
  one: the instructions are a section, and what is missing is invalidation
  when a tool writes to a file the section was built from.
- **Timeout and repeat guards.** Still a tool-pipeline concern and still not
  this lane's, as the previous sweep said. The contract clause that describes
  them (`"timed-out"`, `"repeated"`) is published and unserved.

## 5. Changelog row

| 2026-08-22 | The runtime context a turn tells the model (`crates/turn/src/context.rs`, TC-PORT-RTCTX-1 and TC-RTCTX-2..10), which contract section 4.4.8 has specified and nothing has produced. That is the third published-with-no-producer gap this lane has found, after `assistant/message`'s missing turn and step and section 4.4.9's origin facts, and it had the same shape: a page of contract, a row in the section 4.3.2 payload table, a green boundary case (TC-PROTO-25), and no journal that ever carried one. Providers are named, ordered by the deployment and gathered once per turn between `turn/start` and the first `step/start` - once per turn and not once per step, because a snapshot is a fact about when the turn began, so a step that runs for ten minutes works from the time it started with and a tool that changed directory does not retroactively change what the model was told. It is a user message and never a prompt section: a provider caches a prompt by its longest stable prefix, and a sentence saying what time it is would invalidate that prefix on every request of every session. Only the newest snapshot derives to a message, because a hundred turns write a hundred and yesterday's date is worse than no date; the earlier ones stay on the journal, which records what happened. A deployment that configures no providers pays nothing - not a journal line and not a message - which is why the record is skipped rather than written empty. Two departures from upstream are deliberate: the reading is UTC with the zone stated, because a display zone means a time-zone database and this workspace has none, and the providers share one record rather than appending one message each, because "only the newest travels" needs a single record to be decidable. One reading of the clause was settled by a case rather than by argument: the snapshot enters the history where the journal puts it, before the message that opened the turn, rather than last - both satisfy the caching property, and the journal's order also keeps a request ending in the user's ask or a tool's result, which matters because the thing that answers the last message is not only a model but this crate's own mock, which looped when the block went last. |
