# Parity update: searching a session (slice `query-search`)

Lane: the query half of `session-query/*`. This closes the first of the four clauses
`acp-surfaces.md` left open on that row, and does not touch the other three.

## The row this changes

`session/*`, `session-query/*`. The `Today` column gains search; the `Gap` column loses
"full-text search and its cursors".

- **`Today` gains:** searching a session's words - case-insensitive terms matched over the
  conversation corpus the filter clause already reads, any-of or all-of, narrowable by the
  ordinary event filter, returning a bounded snippet cut on characters, the whole-session
  match count, and an opaque cursor bound to the query that issued it.
- **`Gap` loses:** "full-text search and its cursors". Still absent from that row:
  SQLite-backed indexing, lineage and event tracing, and title snapshots.

## Cases

`TC-PORT-QUERY-20..31`. All offline; none needs a key, a network, or the binary.

## The decision this slice is really about

Upstream labels every search hit with a **surface**: whether the event is still model-visible or
has been replaced by a compaction summary. It is the right field to carry, and for a reason worth
stating rather than inheriting. tetanus never deletes anything to shrink a conversation - the
journal is append-only and `compaction::surface` changes how history is *derived* - so a long
session holds text that is on the log and that the model has since lost. A search that returned
that text unlabelled would show a person a sentence and imply the model can see it.

What this crate will **not** do is work the surface out. `AGENTS.md` is explicit that anything
reading model history goes through the engine's own fold, because a second reader disagrees with
the first the day a session compacts. That left two options and both were wrong:

- Derive it here. That is the second fold the rule forbids, and the disagreement would appear only
  on compacted sessions, which is to say only on the sessions long enough for anyone to search.
- Depend on `tetanus-turn` and call `compaction::surface`. That drags `reqwest` - an HTTP client,
  a TLS stack - into a crate whose stated virtue is that it opens no file, holds no session and
  runs identically in process and over a carrier.

So the surface is an **input**. `Journal::with_surface` takes the seqs the caller's own fold
selected, and a caller that supplies nothing gets `Surface::Unknown` rather than a cheerful
`Current` that nobody checked. `Unknown` and `Current` are deliberately distinct: "we checked and
it is visible" and "we did not check" are different facts, and a surface that defaults to `Current`
is wrong exactly where it matters most. TC-PORT-QUERY-24 pins the property structurally - it passes
only while the surface is an input, and fails the moment someone adds a fold here that "corrects"
the caller.

## Three smaller decisions

1. **No ranking.** Upstream ranks because its provider is SQLite's full-text index, which computes
   a relevance score. A scan has no such number, and inventing one - term counts, field weights -
   would be this crate making up a relevance model no caller asked for and none could tune. Hits
   come back in seq order, which is a fact about the session.
2. **A cursor is bound to its query.** Paging a *different* search with an earlier cursor lines the
   seqs up and produces an answer that looks entirely plausible and is wrong. Every cursor carries
   a fingerprint of the query's shape - terms, all-of, filter, but not page size, because changing
   the page size does not change which events match - and is refused against any other.
3. **A blank search is refused, not answered.** An empty search box means "I have not typed
   anything yet", and the whole session is the least useful possible reply and the most expensive.

## A defect this slice found in itself

The first implementation decided "are there more pages" by counting the *events* before the cursor
rather than the *matches* before it. TC-PORT-QUERY-25 could not see it, because every event in that
case's log was a hit and the two counts agree exactly when that is true. TC-PORT-QUERY-31 puts the
non-matching events *before* the matches, which is where the two diverge, and the bug drops the
final match - a search that silently returns a short answer. Reverting the fix fails that case
with `[5, 7]` against `[5, 7, 8]`; the first draft of the case put its fillers after the matches
and passed against the bug, which is recorded here because a case that cannot fail is worse than
no case.
