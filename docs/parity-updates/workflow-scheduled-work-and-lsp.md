# Parity note: scheduled and background work, plus language intelligence

For folding into [`../parity.md`](../parity.md) by the reconciliation slice.
Written here rather than in place because every lane collides on that file.

Branch: `fm/tetanus-p3-workflow`.
Areas: `workflow/*`, `schedule/*`, `jobs/*` (24 spec files) and `lsp/*` (12).
Both rows read `Today: None` before this branch.

---

## 1. Section 3 rows, as they should now read

### `workflow/*`, `schedule/*`, `jobs/*` (24)

**Today** becomes: a durable job store - an append-only journal of transitions,
with the session log's crash discipline and a reopen that closes what the
process was doing when it died; workflow runs of declared steps whose progress
is on the journal, cancellable at a step boundary and resumable from the record
after a restart; and time-triggered and interval-triggered work with an
anchored grid, a missed recurrence that owes one fire rather than a backlog,
and an explicit answer for a fire that lands on a run still going.

**Gap** becomes: the JavaScript workflow script and its worker-thread realm,
the model-facing tools over all three (`tool-jobs`, `tool-workflow`,
`tool-ralph`, the schedule tools), and delivery of a fired schedule into a
session as a prompt.

**Closes in** stays ③ for the script runtime; the rest is ② and is now served.

### `lsp/*` (12)

**Today** becomes: an stdio language-server client - `Content-Length` framing
bounded at both the header and the body, the initialize/didOpen/query/shutdown
lifecycle over a real subprocess, bounded waits - and the model-facing `lsp`
tool over it, answering definitions, references and diagnostics, with a server
that dies contained as a failed tool call.

**Gap** becomes: a pool of servers reused across calls, and the
document-synchronisation half for unsaved buffers.

---

## 2. Section 4 rows

### New row: `jobs/jobs/tests/service.spec.ts`, `jobs-local/tests/jobs.spec.ts`

Ports to `crates/core/tests/jobs.rs`.

Asserts: the lifecycle a job record moves through, and what a restart does with
work that was live.

part ported: TC-PORT-JOB-1..12 for the lifecycle, a settled job surviving a
restart, a restart closing what was live and saying so, a job that cannot end
twice, only a queued job starting, an unknown job reported, a named id that is
taken, a minted id that never collides, listing in order and by owner, a torn
tail dropped and the file repaired, a damaged committed line refused by line,
and an unsafe name refused.

Upstream's registry is in memory and its durability is the owning agent's
session; persistence is the tetanus difference and the reason the acceptance
case is a restart, so its disposal, reentrancy and agent-teardown cases have no
counterpart. Its `reported` flag suppresses a duplicate completion notice to a
live reader, which is a reporting concern rather than a storage one. Its
`readOutput` consuming cursor belongs to a producer that streams; this stores
the terminal output a producer hands over.

One decision is tetanus's own and belongs in the row: `Interrupted` is a
distinct terminal status from `Failed`. The work reported no failure and nobody
knows how far it got, which is a different thing to tell a user and a different
thing for a scheduler to decide on.

### New row: `schedule/schedule/tests/recurrence.spec.ts`, `runtime.spec.ts`, `domain.spec.ts`, `jsonl-restart.spec.ts`

Ports to `crates/core/tests/schedule.rs`.

Asserts: when work is due, what a missed recurrence owes, and what happens to a
fire that overlaps.

part ported: TC-PORT-SCHED-1..12 for a one-shot firing once, a recurrence on
its anchor grid, a missed day owing one fire, a restart running at the right
time afterwards, the three overlap policies, at most one held fire, a deleted
schedule staying deleted, the creation refusals, a recurrence anchored in the
past owing nothing yet, and the journal's crash rules.

Every case moves the clock rather than sleeping, because the clock is an
argument to every call in the module rather than a global. That is what makes
TC-PORT-SCHED-4 - the acceptance case - a statement about behaviour rather than
a timing test.

Upstream delivers a reminder as a user message into the session that created it
and has one delivery mode; tetanus keeps the payload opaque, because the same
seam has to carry a workflow step and a reminder, so its delivery-framing and
session-liveness cases have nothing to restate. Its local-calendar and IANA
time-zone input is a parsing surface this workspace has no dependency for - a
target arrives here as an instant - so its zone-validation cases have no
counterpart.

The overlap policy is an addition rather than a port: upstream has no answer
for a fire landing on a run still going, and the brief asked for an explicit
one. What is deliberately not offered is the accidental behaviour - firing
anyway and letting two copies race - which is what a scheduler with no opinion
does.

### New row: `workflow/workflow/tests/workflow.spec.ts`, `workflow-worker-thread/tests/workflow-worker-thread.spec.ts`

Ports to `crates/turn/tests/upstream_workflow.rs`.

Asserts: the boundaries a run records, where a cancellation lands, and what a
restart continues from.

part ported: TC-PORT-FLOW-1..10 for the whole boundary sequence, a failing step
ending the run, a cancelled run stopping at its next checkpoint and saying so,
a restart continuing from the record, a resume running a failed step again, a
completed run refusing a resume, a restart telling which runs were in flight,
two runs on one journal staying independent, a workflow with no steps refused,
and an interrupt before the first step starting nothing.

Upstream's workflow is a JavaScript script run in a worker thread whose steps
are the `agent()` calls it makes as it executes. tetanus has no script runtime,
so its parsing, realm isolation, `agent()` concurrency and worker-death paths
have nothing to restate; what ports is the part that is not JavaScript's - the
declared sequence, the durable progress, the cancellation point and the resume.
Its `phase()` grouping is the step's own name here, because a tetanus step is
declared rather than discovered.

The port found one design flaw: `resume` first refused any run with an end on
the journal, which meant it refused precisely the cancelled and failed runs it
exists to continue - both of those write an end too. Only completion refuses
now, and the error is named `AlreadyCompleted` so the code cannot drift back.
TC-PORT-FLOW-4 and -5 are those two resumes.

### New row: `lsp/lsp-stdio/tests/framing.spec.ts`, `lifecycle.spec.ts`, `connection.spec.ts`, `lsp/tool-lsp/tests/tool-lsp.spec.ts`

Ports to `crates/turn/tests/upstream_lsp.rs`.

Asserts: the base protocol's framing, the query lifecycle over a real
subprocess, and that a server which dies is a failed tool call.

part ported: TC-PORT-LSP-1..16 for framing and its bounds, a message split
anywhere, several in one chunk, a real query over stdio, references and pushed
diagnostics, a clean file answering none rather than timing out, a server that
dies contained with its own words, a server that never answers bounded, a
missing program naming itself, a file outside the workspace refused, the tool's
one-based coordinates, a dead server as a failed call, the argument refusals,
an empty answer that says so, and one case against real `rust-analyzer`.

The suite's server is a script written into the test file, so it is offline and
deterministic and its behaviour is readable beside the cases that depend on it.
TC-PORT-LSP-16 is what stops that mock becoming the specification; it asserts
the lifecycle rather than the answer, because rust-analyzer replies only once
it has built the crate graph and requiring a hit would make it a test of a
third-party program's indexing speed under load. It reports itself skipped when
the binary is absent, the rule the one live provider case already follows.

Upstream pools servers by language and reuses one across calls, so its
eviction, idle-timeout and concurrent-borrow cases have no counterpart: this
opens a server per query and closes it, which is slower and has no lifecycle to
get wrong. Its document-synchronisation half is unrepresentable - tetanus has
no editor buffers, so the file on disk is the document.

---

## 3. Changelog rows

For [`../parity-changelog.md`](../parity-changelog.md), which is append-only and
`merge=union`. Reproduced verbatim so the reconciliation slice appends rather
than rewrites.

| 2026-08-21 | A durable job store (`crates/core/src/jobs.rs`, TC-PORT-JOB-1..12), the first piece of the `jobs/*` row and the one the schedule and workflow slices stand on. It is an append-only journal rather than a table of rows, for the reason the session log is one: a record of what happened cannot be corrupted by a later write, a crash can only cut the last line, and the state is a fold anyone can re-derive - a mutable row per job gives a torn write no way to be detected. Reopening repairs with the session store's discipline: a job the log last saw live cannot still be live, so `open` closes it as `Interrupted` and *appends* that closure, which is why a second reopen has nothing left to do and the file has not grown. `Interrupted` is deliberately not `Failed`: the work reported no failure and nobody knows how far it got, which is a different thing to tell a user and a different thing for a scheduler to decide on. A job ends exactly once, upstream's `reported` rule reached by a different route - a second terminal record makes "how did this end" a question with two answers. |
| 2026-08-21 | Time-triggered work (`crates/core/src/schedule.rs`, TC-PORT-SCHED-1..12), opening the `schedule/*` row. The clock is an argument to every call rather than a global, which is what lets the whole suite move time instead of sleeping - no case waits for anything - and what makes a restart honest, since a process that comes back asks what is due now and gets the answer the dead one would have given. A missed recurrence fires once and realigns to its anchor: a harness down for a day owes one sweep, not twenty-four, because catching up floods a session with stale work the moment it returns, and drifting the anchor lets a job set for the top of the hour wander off it. The overlap policy is an addition rather than a port - upstream has no answer for a fire landing on a run still going - and it deliberately omits the accidental behaviour, firing anyway and letting two copies race, which is what a scheduler with no opinion does. `Queue` holds at most one, because a backlog of identical work is the thing a scheduler must not build. Whether the previous run is still going is the caller's to supply: only the caller knows when its own work ended. |
| 2026-08-21 | Workflow runs (`crates/turn/src/workflow.rs`, TC-PORT-FLOW-1..10), opening the `workflow/*` row for everything that is not upstream's script runtime. A turn is one exchange with a model; a migration or a sweep is a sequence of named steps that outlives one, and has to survive a restart in the middle. The journal is the progress record rather than a status field, so what a surface renders, what a restart reads and what a case asserts are the same events - progress in memory is progress a crash erases, and the work a crash must not erase is what a workflow is for. Cancellation lands at the next step boundary, the turn engine's rule for the same reason: abandoning a running step would leave the journal claiming work that neither finished nor failed. Resuming is re-reading, so a completed step is never run twice - the steps are not assumed idempotent, because most useful ones are not. The port found one design flaw: `resume` refused any run with an end on the journal, which is precisely the cancelled and failed runs it exists to continue, since both write an end too. Only completion refuses now. |
| 2026-08-21 | A language-server client and its tool (`crates/turn/src/lsp/`, TC-PORT-LSP-1..16), opening the `lsp/*` row. Textual search finds every `foo` and a language server knows which one, which is worth a subprocess before a change and worth nothing for ordinary navigation - so the tool says which is which rather than leaving the model to guess. The rule the module is arranged around is that a server which dies is a failed tool call and never a dead turn: a language server is a large third-party program that crashes, hangs and gets OOM-killed, and every one of those becomes a `ToolError` carrying the server's own dying words, because "install the toolchain" and "the tool failed" are different things to tell a user. Every wait is bounded, since a server that accepts a request and goes quiet is this class of program's ordinary failure and an unbounded wait is a hung turn. The framing decoder bounds the header and the body, because a decoder that trusts a length field is one a corrupt stream can take the harness down with. Diagnostics are pushed rather than requested, so silence about a file reads as nothing wrong instead of as a timeout - the case treating every operation as a round trip gets wrong. The suite's server is a script in the test file, offline and deterministic; one case runs against real rust-analyzer to stop that mock becoming the specification, and asserts the lifecycle rather than the answer, because requiring a hit would make it a test of a third-party program's indexing speed under load. |

---

## 5. Written two days later, when this branch was rescued

This note was written against a master that has since moved 200 commits. Three
things it says need a sentence each, and one row edit changes.

**The `lsp` tool is composed now, and was not when this was written.**
`crates/toolset` did not exist then, so the tool was reachable only by a
composer writing Rust - the same state every tool crate was in before the
assembly landed. It is now one entry in `sources()`, declared and empty unless
a document names `lsp.server`, because the client drives any server that speaks
the protocol and which one belongs to the project rather than to the harness
(TC-TOOLSET-11). The `lsp/*` row's **Today** should therefore end: *and the
tool composed into the shipped binary when a document names the server to run*.

**The job store has a named consumer waiting on it, and this does not wire
it.** The process lane closed its own row leaving exactly one clause open:
`run_in_background` for the one-shot `shell` tool, which it declined to build
because "a second store built here would leave that row with two to reconcile".
That store is this one. The wiring is the exec lane's - a `run_in_background`
argument that starts the same `proc::Command` without awaiting it and hands the
group id to the store - and the `shell/*` row's gap clause should now name this
store rather than a store that does not exist.

**Delivering a fired schedule into a session is no longer blocked.** The gap
clause above says a fire cannot reach a session as a prompt; the queue it
needed landed in the meantime (`crates/turn/src/inbox.rs`, TC-INBOX-1..16),
which holds a message until a boundary claims it. So that clause changes from a
missing mechanism to an unwired one: what is left is the caller that puts a
fired schedule on the inbox, and the question of which session a schedule
belongs to, which nothing yet answers.

**Nothing in the five commits stopped fitting.** The client, the stores and the
workflow runs all compile and pass against this master unchanged - 50 cases,
`crates/core/tests/{jobs,schedule}.rs` and `crates/turn/tests/{upstream_workflow,upstream_lsp}.rs` -
and the only edits the rescue needed were three module lists and three
documentation tables that had grown new rows underneath it.
